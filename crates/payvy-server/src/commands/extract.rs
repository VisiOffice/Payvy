//! Reading a bill from an inbox document. The model only reads; everything
//! it says is checked here (`payvy_domain::extraction`), matched against the
//! vendors on file, and kept as a draft. Nothing becomes a bill until a
//! person or agent calls `bill.create`, quoting the extraction.

use payvy_domain::bills::invoice_key;
use payvy_domain::documents as doc_rules;
use payvy_domain::extraction::{self as rules, Check, Draft, VendorMatch};
use payvy_store::{self as store, Extraction, ExtractionRead, NewExtraction};
use serde::Deserialize;
use serde_json::{Value, json};
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
    let extractor = cx.extractor()?;
    let storage = cx.storage()?;
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
    // Step 1 of page arrangement: the model still reads the original file,
    // but the reading records which arrangement it was made under, so a
    // later change marks it out of date (docs/document-pages.md).
    let arrangement_version = super::documents::current_arrangement(cx, doc.id)
        .await?
        .map_or(0, |a| a.version);
    if cx.preview {
        // Reading costs money and takes a while, so a preview says what
        // would happen instead.
        return Ok(
            json!({ "would_read": doc.id, "provider": provider, "model": extractor.model() }),
        );
    }

    let read = async {
        let bytes = storage
            .get(&doc.storage_key)
            .await
            .map_err(|e| format!("couldn't fetch the file: {e:#}"))?;
        let kind = doc_rules::check(&bytes).map_err(|e| e.to_string())?;
        extractor
            .read(&bytes, kind)
            .await
            .map_err(|e| e.to_string())
    }
    .await;

    let row = match read {
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
            let sender = match doc.email_id {
                Some(id) => store::get_email(&mut cx.tx, id)
                    .await?
                    .and_then(|e| e.from_address),
                None => None,
            };
            let candidates = store::vendor_candidates(&mut cx.tx).await?;
            let matched = rules::match_vendor(&draft, sender.as_deref(), &candidates);
            vendor_checks(cx, &draft, &matched, &mut checks).await?;
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
                json!({ "document_id": doc.id, "extraction_id": row.id, "model": row.model, "ready": ready }),
            )
            .await?;
            row
        }
    };
    Ok(summary(&row))
}

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
    let ready = row.status == "read" && failed.is_empty();
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
        "ready": ready,
        "failed_checks": failed,
        "bill_input": bill_input,
        "new_vendor": new_vendor,
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
