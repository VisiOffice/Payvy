//! Reading a bill from an inbox document. The model only reads; everything
//! it says is checked here (`payvy_domain::extraction`), matched against the
//! vendors on file, and kept as a draft. Nothing becomes a bill until a
//! person or agent calls `bill.create`, quoting the extraction.

use payvy_domain::bills::invoice_key;
use payvy_domain::documents as doc_rules;
use payvy_domain::extraction::{self as rules, Check, Draft, VendorMatch};
use payvy_domain::invoice_evidence as evidence;
use payvy_store::{self as store, ActorKind, Extraction, ExtractionRead, NewExtraction};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{CommandError, Cx};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extract {
    document_id: Uuid,
    #[serde(default)]
    again: bool,
}

pub async fn extract(cx: &mut Cx<'_>, input: Extract) -> Result<Value, CommandError> {
    // Before anything else: on a plan without the built-in reader, Payvy
    // makes no model calls for this company, retries and split parts included.
    cx.require(crate::entitlements::Need::Reader).await?;
    let extractor = cx.extractor()?;
    let storage = cx.storage()?;
    store::lock_invoice_settings(&mut cx.tx, cx.org).await?;
    let doc = store::get_document(&mut cx.tx, input.document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    if doc.status != "inbox" {
        return Err(CommandError::Conflict(format!(
            "this document is {}, not in the inbox",
            doc.status
        )));
    }
    let previous = store::document_extractions(&mut cx.tx, doc.id).await?;
    if !input.again
        && let Some(done) = previous.iter().find(|e| e.status == "read")
    {
        return Err(CommandError::Conflict(format!(
            "this document was already read by {} (extraction {}); pass again: true to read it once more",
            done.model, done.id
        )));
    }
    let provider = extractor.provider_label();
    // The model reads the original file, told which of its pages are this
    // document's when that isn't all of them (docs/document-pages.md). The
    // reading records the arrangement it was made under, so a later change
    // marks it out of date.
    let arrangement_version = super::documents::current_arrangement(cx, doc.id)
        .await?
        .map_or(0, |a| a.version);
    let (doc_pages, hint) = super::documents::pages_to_read(cx, &doc).await?;
    if cx.preview {
        // Reading costs money and takes a while, so a preview says what
        // would happen instead.
        return Ok(
            json!({ "would_read": doc.id, "provider": provider, "model": extractor.model() }),
        );
    }

    let read_token = Uuid::new_v4();
    if !store::claim_extraction_read(&mut cx.tx, doc.id, read_token, cx.now).await? {
        return Err(CommandError::Conflict("This document is already being read. If the reader stopped unexpectedly, retry after ten minutes.".into()));
    }
    cx.extraction_claim = Some((doc.id, read_token));
    let v2 = store::extraction_v2_enabled(&mut cx.tx).await?
        || store::extraction_v2_enrolled(&mut cx.tx, doc.id).await?;
    if v2 && store::enroll_extraction_v2(&mut cx.tx, doc.id).await? {
        cx.emit(
            "document.extraction_v2_enrolled",
            doc.id,
            json!({"schema_version":2}),
        )
        .await?;
    }
    let started_at = (cx.clock)();
    let timer = std::time::Instant::now();
    // Release document/business locks before the provider or local OCR runs.
    // The new transaction has no business locks. Re-authorize and re-read below.
    let next = cx.store.begin(cx.org).await?;
    let previous = std::mem::replace(&mut cx.tx, next);
    previous.commit().await.map_err(anyhow::Error::from)?;
    let mut source_pages = Vec::new();
    let actor_kind = cx.actor.kind;
    let read = async {
        let bytes = storage
            .get(&doc.storage_key)
            .await
            .map_err(|e| format!("couldn't fetch the file: {e:#}"))?;
        let kind = doc_rules::check(&bytes).map_err(|e| e.to_string())?;
        if v2 && (doc_pages.is_empty() || doc_pages.len() > 24) {
            return Err(
                "Extraction v2 supports at most 24 selected pages. Split only at verified invoice boundaries; keep a single longer invoice intact for manual review. Automatic page batching is not available yet.".into(),
            );
        }

        // Write a durable attempt row before calling the provider so usage and
        // cost survive parse/validation failures or downstream DB failures.
        let attempt_id = {
            let mut utx = cx
                .store
                .begin(cx.org)
                .await
                .map_err(|e| format!("couldn't record usage attempt: {e:#}"))?;
            let id = store::start_ai_usage_attempt(
                &mut utx,
                cx.org,
                &store::NewAiUsageAttempt {
                    document_id: doc.id,
                    actor_kind,
                    actor_name: &cx.actor.name,
                    provider: &provider,
                    requested_model: extractor.model(),
                    started_at: (cx.clock)(),
                },
            )
            .await
            .map_err(|e| format!("couldn't record usage attempt: {e:#}"))?;
            utx.commit()
                .await
                .map_err(|e| format!("couldn't record usage attempt: {e:#}"))?;
            id
        };

        let attempt = if v2 {
            let (attempt, pages) = tokio::join!(
                extractor.read_pages_v2_tracked(&bytes, kind, &doc_pages),
                crate::source_text::read(&bytes, kind, &doc_pages)
            );
            source_pages = pages;
            attempt
        } else {
            extractor
                .read_pages_tracked(&bytes, kind, hint.as_deref())
                .await
        };

        let err_msg = attempt.outcome.fixed_error_message();
        let priced_model = attempt
            .telemetry
            .actual_model
            .as_deref()
            .unwrap_or_else(|| extractor.model());
        let priced = payvy_domain::ai_usage::compute_cost_microcents(
            priced_model,
            &attempt.telemetry.usage,
        );
        let (pricing_version, cost_microcents) = match priced {
            Some((v, c)) => (Some(v), Some(c)),
            None => (None, None),
        };
        if let Ok(mut utx) = cx.store.begin(cx.org).await {
            if let Err(e) = store::complete_ai_usage_attempt(
                &mut utx,
                attempt_id,
                &store::CompleteAiUsageAttempt {
                    outcome: attempt.outcome.as_str(),
                    actual_model: attempt.telemetry.actual_model.as_deref(),
                    provider_request_id: attempt.telemetry.provider_request_id.as_deref(),
                    error: err_msg,
                    usage: attempt.telemetry.usage,
                    pricing_version,
                    cost_microcents,
                    completed_at: (cx.clock)(),
                },
            )
            .await
            {
                tracing::warn!(attempt = %attempt_id, "couldn't complete usage attempt: {e:#}");
            } else if let Err(e) = utx.commit().await {
                tracing::warn!(attempt = %attempt_id, "couldn't commit usage attempt: {e:#}");
            }
        }

        attempt.result.map_err(|e| e.to_string())
    }
    .await;

    cx.roles = crate::permissions::roles(&mut cx.tx, cx.actor).await?;
    if !cx.can("document.extract") {
        return Err(CommandError::Forbidden(
            "Document extraction permission changed while reading".into(),
        ));
    }
    let current = store::get_document(&mut cx.tx, doc.id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    let current_version = super::documents::current_arrangement(cx, doc.id)
        .await?
        .map_or(0, |a| a.version);
    let current_read = store::extraction_read_is_current(&mut cx.tx, doc.id, read_token).await?;
    let stale =
        !current_read || current_version != arrangement_version || current.status != "inbox";
    store::finish_extraction_read(&mut cx.tx, doc.id, read_token).await?;
    let mut analysis = json!({"schema_version":if v2 {2}else{1},"prompt_version":if v2 {evidence::PROMPT_VERSION}else{"invoice-v1"},
        "prompt_hash":hex::encode(Sha256::digest(if v2 {evidence::INSTRUCTIONS}else{rules::INSTRUCTIONS})),
        "schema_hash":hex::encode(Sha256::digest(serde_json::to_vec(&if v2 {rules::schema_v2()}else{rules::schema()}).unwrap())),
        "source_sha256":doc.sha256,"selected_pages":doc_pages,"arrangement_version":arrangement_version,
        "started_at":started_at,"completed_at":(cx.clock)(),"duration_ms":timer.elapsed().as_millis(),"stale":stale});
    let mut found = Vec::new();
    let mut row = match read {
        Err(error) => {
            let row = store::insert_extraction(
                &mut cx.tx,
                cx.org,
                &NewExtraction {
                    document_id: doc.id,
                    provider: &provider,
                    model: extractor.model(),
                    outcome: Err(&error),
                    created_at: cx.now,
                    created_by: &cx.actor.name,
                    arrangement_version,
                },
            )
            .await?;
            cx.emit(
                "document.extraction_failed",
                doc.id,
                json!({ "document_id": doc.id, "extraction_id": row.id, "error": error }),
            )
            .await?;
            row
        }
        Ok(out) => {
            let (draft, mut checks) = rules::interpret(&out.read);
            if stale {
                checks.push(Check{check:"stale_review".into(),ok:false,detail:"Pages or document status changed while reading; this result is history only. Read the current pages again.".into()});
            }
            if v2 {
                let coverage_ok = out.read.coverage.as_ref().is_some_and(|c| {
                    let mut actual = c.pages_read.clone();
                    actual.sort_unstable();
                    let mut expected = doc_pages.clone();
                    expected.sort_unstable();
                    c.complete && actual == expected
                });
                checks.push(Check {
                    check: "page_coverage_incomplete".into(),
                    ok: coverage_ok,
                    detail: if coverage_ok {
                        "Reader reports all selected pages inspected"
                    } else {
                        "Reader did not inspect every selected page completely; check missing pages"
                    }
                    .into(),
                });
                let rows: Vec<_> = out
                    .read
                    .lines
                    .iter()
                    .map(|l| {
                        let d = l.details.clone().unwrap_or_default();
                        let mut fields = vec![
                            ("description", Some(l.description.as_str())),
                            ("amount", Some(l.amount.as_str())),
                        ];
                        fields.extend(d.fields());
                        fields
                            .into_iter()
                            .filter_map(|(field, value)| {
                                value.map(|v| {
                                    evidence::ground(field, v, &l.description, &d, &source_pages)
                                })
                            })
                            .flatten()
                            .collect::<Vec<_>>()
                    })
                    .collect();
                analysis["coverage"] = json!(out.read.coverage);
                analysis["coverage_ok"] = json!(coverage_ok);
                analysis["source_pages"] = json!(source_pages);
                analysis["rows"] = json!(rows);
            }
            let sender = match doc.email_id {
                Some(id) => store::get_email(&mut cx.tx, id)
                    .await?
                    .and_then(|e| e.from_address),
                None => None,
            };
            let candidates = store::vendor_candidates(&mut cx.tx).await?;
            let matched = rules::match_vendor(&draft, sender.as_deref(), &candidates);
            vendor_checks(cx, &draft, &matched, &mut checks).await?;
            // Several invoices in one file: where each is, if the pages
            // the reader named are this document's, distinct and real.
            let invoices = if doc_pages.is_empty() {
                match out.read.invoices.as_deref() {
                    Some(list) if list.len() > 1 => {
                        Err("this file can't be read page by page".to_owned())
                    }
                    _ => Ok(Vec::new()),
                }
            } else {
                rules::invoices(&out.read, &doc_pages)
            };
            checks.extend(rules::invoices_check(&invoices));
            found = invoices.unwrap_or_default();
            let invoices_json = (!found.is_empty()).then(|| json!(found));
            let row = store::insert_extraction(
                &mut cx.tx,
                cx.org,
                &NewExtraction {
                    document_id: doc.id,
                    provider: &provider,
                    model: &out.model,
                    outcome: Ok(ExtractionRead {
                        answer: &out.raw,
                        draft: &json!(draft),
                        checks: &json!(checks),
                        vendor_match: &json!(matched),
                        invoices: invoices_json.as_ref(),
                    }),
                    created_at: cx.now,
                    created_by: &cx.actor.name,
                    arrangement_version,
                },
            )
            .await?;
            let ready = checks.iter().all(|c| c.ok);
            cx.emit(
                "document.extracted",
                doc.id,
                json!({ "document_id": doc.id, "extraction_id": row.id, "model": row.model, "ready": ready,
                        "invoices": found.len().max(1) }),
            )
            .await?;
            row
        }
    };
    store::set_extraction_analysis(&mut cx.tx, row.id, &analysis).await?;
    row.analysis = Some(analysis);
    let mut out = summary(&row);
    // An agent's reading (the background reader's too) splits a file of
    // several invoices straight away, one document per invoice, under the
    // agent's name; it can be undone. A person on the page gets the
    // suggestion instead. Only a file that arrived is split this way: a
    // document already split from one never is, so this can't loop.
    // The groups come from the model, and an emailed file is untrusted: a
    // file claiming hundreds of invoices would become hundreds of documents,
    // each read again at a model call apiece. Past MAX_AUTO_SPLIT it stays
    // a suggestion for a person.
    let automatic = !stale && cx.actor.kind != ActorKind::Person && doc.derived_from.is_none();
    if found.len() > MAX_AUTO_SPLIT && automatic {
        out["split"] = json!({ "error": format!(
            "the reader found {} invoices; more than {MAX_AUTO_SPLIT} are left for a person to split",
            found.len()
        ) });
    } else if found.len() > 1 && automatic {
        let groups: Vec<Vec<u32>> = found.iter().map(|f| f.pages.clone()).collect();
        // The split checks everything before it writes, so a refusal
        // leaves the reading as it is, with the reason.
        match super::documents::split_pages(cx, &doc, Some(arrangement_version), &groups).await {
            Ok(parts) => {
                let ids: Vec<Uuid> = parts.iter().map(|d| d.id).collect();
                out["split"] = json!({
                    "documents": parts,
                    "note": "Each new document is read on its own: the background reader picks them up, or call document.extract on each.",
                    "to_read": ids,
                });
            }
            Err(CommandError::Invalid(e) | CommandError::Conflict(e)) => {
                out["split"] = json!({ "error": e });
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// The most documents an agent's reading splits a file into on its own.
pub const MAX_AUTO_SPLIT: usize = 20;

/// Adds the vendor and duplicate checks, which need the database.
async fn vendor_checks(
    cx: &mut Cx<'_>,
    draft: &Draft,
    matched: &VendorMatch<Uuid>,
    checks: &mut Vec<Check>,
) -> Result<(), CommandError> {
    let (ok, detail) = match matched {
        VendorMatch::Matched { reasons, .. } => (true, reasons.join("; ")),
        VendorMatch::Ambiguous { vendor_ids } => (
            false,
            format!(
                "{} vendors on file fit equally well; pick one",
                vendor_ids.len()
            ),
        ),
        VendorMatch::NoMatch => (
            false,
            match &draft.vendor_name {
                Some(n) => format!("no vendor on file matches {n}; create it with vendor.create"),
                None => "no vendor name found".into(),
            },
        ),
    };
    checks.push(Check {
        check: "vendor".into(),
        ok,
        detail,
    });
    if let (VendorMatch::Matched { vendor_id, .. }, Some(number)) =
        (matched, draft.invoice_number.as_deref())
        && let Some(existing) =
            store::bill_with_invoice_key(&mut cx.tx, *vendor_id, &invoice_key(number)).await?
    {
        checks.push(Check {
            check: "not_a_duplicate".into(),
            ok: false,
            detail: format!("bill {existing} from this vendor already has invoice number {number}"),
        });
    }
    Ok(())
}

/// An extraction as callers see it: the record, whether it can become a
/// bill as it stands, and the `bill.create` input that would do it.
pub(super) fn summary(row: &Extraction) -> Value {
    let draft: Option<Draft> = row
        .draft
        .clone()
        .and_then(|d| serde_json::from_value(d).ok());
    let checks: Vec<Check> = row
        .checks
        .clone()
        .and_then(|c| serde_json::from_value(c).ok())
        .unwrap_or_default();
    let matched: Option<VendorMatch<Uuid>> = row
        .vendor_match
        .clone()
        .and_then(|m| serde_json::from_value(m).ok());
    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| !c.ok)
        .map(|c| c.check.as_str())
        .collect();
    let review_required = row
        .analysis
        .as_ref()
        .is_some_and(|a| a["schema_version"] == 2);
    let ready = row.status == "read" && failed.is_empty() && !review_required;
    let bill_input = draft
        .as_ref()
        .and_then(|d| bill_input(row, d, matched.as_ref()));
    let new_vendor = match (&matched, &draft) {
        (Some(VendorMatch::NoMatch), Some(d)) if d.vendor_name.is_some() => {
            Some(json!({ "name": d.vendor_name, "email": d.vendor_email }))
        }
        _ => None,
    };
    json!({
        "extraction": row,
        "invoices": row.invoices.clone().unwrap_or_else(|| json!([])),
        "ready": ready,
        "review_required": review_required,
        "failed_checks": failed,
        "bill_input": bill_input,
        "new_vendor": new_vendor,
    })
}

/// What a reading found, compact enough for one row of a list: who it is
/// from, how many invoices and for how much, and what still needs a person.
pub(super) fn glance(row: &Extraction) -> Value {
    let full = summary(row);
    let draft: Option<Draft> = row
        .draft
        .clone()
        .and_then(|d| serde_json::from_value(d).ok());
    let matched: Option<VendorMatch<Uuid>> = row
        .vendor_match
        .clone()
        .and_then(|m| serde_json::from_value(m).ok());
    let found: Vec<Value> = row
        .invoices
        .as_ref()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (vendor_name, vendor_id, vendor_match) = match &matched {
        Some(VendorMatch::Matched {
            vendor_id,
            name,
            confidence,
            ..
        }) => (
            Some(name.clone()),
            Some(*vendor_id),
            json!({ "result": "matched", "confidence": confidence }),
        ),
        Some(VendorMatch::Ambiguous { .. }) => (None, None, json!({ "result": "ambiguous" })),
        Some(VendorMatch::NoMatch) => (None, None, json!({ "result": "new" })),
        None => (None, None, Value::Null),
    };
    let printed = draft.as_ref().and_then(|d| d.vendor_name.clone());
    // Several invoices: their own numbers and the sum of their totals. One:
    // what the reading drafted.
    let (numbers, total_cents): (Vec<Value>, Option<i64>) = if found.len() > 1 {
        let totals: Option<i64> = found.iter().map(|i| i["total_cents"].as_i64()).sum();
        (
            found.iter().map(|i| i["invoice_number"].clone()).collect(),
            totals,
        )
    } else {
        (
            draft
                .as_ref()
                .and_then(|d| d.invoice_number.clone())
                .map(Value::from)
                .into_iter()
                .collect(),
            draft.as_ref().and_then(|d| d.total_cents),
        )
    };
    json!({
        "status": row.status,
        "kind": draft.as_ref().map(|d| d.kind),
        "vendor": {
            "id": vendor_id,
            "name": vendor_name.clone().or(printed.clone()),
            "printed": printed,
            "match": vendor_match,
        },
        "invoice_count": if found.len() > 1 { found.len() } else { usize::from(!numbers.is_empty()) },
        "invoice_numbers": numbers,
        "total_cents": total_cents,
        "currency": draft.as_ref().and_then(|d| d.currency.clone()),
        "invoice_date": draft.as_ref().and_then(|d| d.invoice_date),
        "due_date": draft.as_ref().and_then(|d| d.due_date),
        "failed_checks": full["failed_checks"],
        "ready": full["ready"],
        "review_required": full["review_required"],
        "read_at": row.created_at,
    })
}

/// The `bill.create` input this extraction suggests, once a vendor is known
/// and the essentials were read.
fn bill_input(row: &Extraction, d: &Draft, matched: Option<&VendorMatch<Uuid>>) -> Option<Value> {
    let Some(VendorMatch::Matched { vendor_id, .. }) = matched else {
        return None;
    };
    let invoice_date = d.invoice_date?;
    let mut input = json!({
        "vendor_id": vendor_id,
        "invoice_number": d.invoice_number.as_ref()?,
        "invoice_date": invoice_date,
        "amount_cents": d.total_cents.filter(|t| *t > 0)?,
        "document_id": row.document_id,
        "extraction_id": row.id,
    });
    if let Some(due) = d.due_date.filter(|due| *due >= invoice_date) {
        input["due_date"] = json!(due);
    }
    if let Some(lines) = rules::bill_lines(d) {
        input["lines"] = json!(lines);
    }
    if let Some(po) = &d.po_number {
        input["memo"] = json!(format!("PO {po}"));
    }
    Some(input)
}

/// Which fields of a new bill differ from what the extraction suggested:
/// the record of what a reviewer corrected.
pub(super) async fn corrections(
    cx: &mut Cx<'_>,
    extraction_id: Uuid,
    document_id: Option<Uuid>,
    entered: &Value,
) -> Result<Vec<String>, CommandError> {
    let row = store::get_extraction(&mut cx.tx, extraction_id)
        .await?
        .ok_or(CommandError::NotFound("extraction"))?;
    if document_id != Some(row.document_id) {
        return Err(CommandError::Invalid(format!(
            "extraction {} is of document {}; pass that as document_id",
            row.id, row.document_id
        )));
    }
    if row.status != "read" {
        return Err(CommandError::Invalid(format!(
            "extraction {} failed and has nothing to enter",
            row.id
        )));
    }
    let suggested = summary(&row)["bill_input"].clone();
    Ok([
        "vendor_id",
        "invoice_number",
        "invoice_date",
        "due_date",
        "amount_cents",
        "lines",
    ]
    .into_iter()
    .filter(|f| {
        let (a, b) = (&entered[*f], &suggested[*f]);
        // Leaving out an optional field isn't correcting it.
        !(a.is_null() && *f != "vendor_id") && a != b
    })
    .map(str::to_owned)
    .collect())
}
