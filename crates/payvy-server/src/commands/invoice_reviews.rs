use super::{CommandError, Cx, documents};
use payvy_domain::invoice_evidence::{self as evidence, SourcePage};
use payvy_domain::{
    extraction,
    invoice_review::{self as rules, Review, Row, TaxBasis},
};
use payvy_store::{self as store, InvoiceReview};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Get {
    document_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configure {
    enabled: bool,
    extraction_v2_enabled: Option<bool>,
}

pub async fn settings(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    Ok(
        json!({"enabled": store::invoice_review_enabled(&mut cx.tx).await?, "extraction_v2_enabled":store::extraction_v2_enabled(&mut cx.tx).await?}),
    )
}
pub async fn configure(cx: &mut Cx<'_>, input: Configure) -> Result<Value, CommandError> {
    store::lock_invoice_settings(&mut cx.tx, cx.org).await?;
    let was = store::invoice_review_enabled(&mut cx.tx).await?;
    let old_v2 = store::extraction_v2_enabled(&mut cx.tx).await?;
    let v2 = input.enabled && input.extraction_v2_enabled.unwrap_or(old_v2);
    if was == input.enabled && old_v2 == v2 {
        return Err(CommandError::Conflict(
            "Line review setting is unchanged".into(),
        ));
    }
    store::set_invoice_review_enabled(&mut cx.tx, cx.org, input.enabled).await?;
    store::set_extraction_v2_enabled(&mut cx.tx, cx.org, v2).await?;
    cx.emit(
        "invoice_review.configured",
        cx.org,
        json!({"was":was,"enabled":input.enabled,"extraction_v2_was":old_v2,"extraction_v2_enabled":v2}),
    )
    .await?;
    settings(cx).await
}
async fn result(cx: &mut Cx<'_>, r: &InvoiceReview) -> Result<Value, CommandError> {
    let (validation, analysis) = validate(cx, r.document_id, &r.payload, r.extraction_id).await?;
    Ok(json!({"review":r,"validation":validation,"analysis":analysis}))
}
async fn validate(
    cx: &mut Cx<'_>,
    doc: Uuid,
    payload: &Review,
    extraction: Option<Uuid>,
) -> Result<(rules::Validation, Option<Value>), CommandError> {
    let mut validation = rules::validate(payload);
    let reading = match extraction {
        Some(id) => store::get_extraction(&mut cx.tx, id).await?,
        None => None,
    };
    let analysis = reading.as_ref().and_then(|e| e.analysis.clone());
    let is_v2 = analysis.as_ref().is_some_and(|a| a["schema_version"] == 2);
    let missing_reading = !is_v2 && store::extraction_v2_enrolled(&mut cx.tx, doc).await?;
    if !is_v2 && !missing_reading {
        return Ok((validation, analysis));
    }
    let data = if is_v2 {
        analysis.clone().unwrap_or(Value::Null)
    } else {
        json!({"schema_version":2,"coverage_ok":false,"coverage":{"reason":"No current richer reading is available. Read again or check all selected pages manually."}})
    };
    if (data["coverage_ok"] != true || data["stale"] == true)
        && payload
            .coverage_resolution
            .as_ref()
            .is_none_or(|s| s.trim().is_empty())
    {
        validation.issues.push(rules::Issue{code:"page_coverage_incomplete",row_id:None,detail:"The reader did not establish complete page coverage. Check every selected page against the original and record what you confirmed, or read again.".into()});
        validation.header_ready = false;
    }
    let pages: Vec<SourcePage> =
        serde_json::from_value(data["source_pages"].clone()).unwrap_or_default();
    let mut resolved_rows = Vec::new();
    for row in &payload.rows {
        if row.excluded_reason.is_some() {
            resolved_rows.push(json!([]));
            continue;
        }
        let details = row.details.clone().unwrap_or_default();
        if let Some(original) = row.source_index.and_then(|i| {
            reading
                .as_ref()
                .and_then(|e| e.answer.as_ref())
                .and_then(|a| a["lines"].get(i))
        }) {
            let old: evidence::LineDetails =
                serde_json::from_value(original["details"].clone()).unwrap_or_default();
            let omitted = old.fields().iter().any(|(f, v)| {
                v.is_some()
                    && details
                        .fields()
                        .iter()
                        .any(|(k, new)| f == k && new.is_none())
            });
            let lost_uncertainty = old.states.iter().any(|(f, state)| {
                matches!(
                    state,
                    evidence::FieldState::Unreadable | evidence::FieldState::Conflicting
                ) && details.states.get(f) != Some(state)
            });
            if (omitted || lost_uncertainty)
                && row
                    .source_resolution
                    .as_ref()
                    .is_none_or(|s| s.trim().is_empty())
            {
                validation.issues.push(rules::Issue{code:"source_detail_removed",row_id:Some(row.id.clone()),detail:"An extracted detail or uncertainty was removed. Explain what you verified against the original.".into()});
            }
        }

        let mut fields = vec![
            ("description", Some(row.description.as_str())),
            ("amount", Some(row.amount.as_str())),
        ];
        fields.extend(details.fields());
        let grounded: Vec<_> = fields
            .into_iter()
            .filter_map(|(f, v)| {
                v.map(|v| evidence::ground(f, v, &row.description, &details, &pages))
            })
            .flatten()
            .collect();
        if grounded.iter().any(|g| g.status != "located")
            && row
                .source_resolution
                .as_ref()
                .is_none_or(|s| s.trim().is_empty())
        {
            validation.issues.push(rules::Issue{code:"source_unverified",row_id:Some(row.id.clone()),detail:"Some row fields could not be located with their row context. Check the cited pages and record a source resolution; text location alone never verifies correctness.".into()});
        }
        resolved_rows.push(json!(grounded));
    }
    validation.ready = validation.issues.is_empty();
    let mut analysis = data;
    if !analysis.is_object() {
        analysis = json!({"schema_version":2,"coverage_ok":false});
    }
    analysis["rows"] = json!(resolved_rows);
    // Full source text stays in the extraction record; the review panel needs only quotes/status.
    analysis.as_object_mut().unwrap().remove("source_pages");
    Ok((validation, Some(analysis)))
}
fn initial(ex: Option<&store::Extraction>) -> Review {
    let answer = ex.and_then(|e| e.answer.as_ref());
    let field = |k: &str| answer.and_then(|a| a[k].as_str()).unwrap_or("").to_owned();
    let vendor_id = ex
        .and_then(|e| e.vendor_match.as_ref())
        .and_then(|v| v["vendor_id"].as_str())
        .map(str::to_owned);
    let rows = answer
        .and_then(|a| a["lines"].as_array())
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, l)| Row {
            id: format!("source-{i}"),
            source_index: Some(i),
            description: l["description"].as_str().unwrap_or("").into(),
            amount: l["amount"].as_str().unwrap_or("").into(),
            excluded_reason: None,
            details: l
                .get("details")
                .and_then(|d| serde_json::from_value(d.clone()).ok()),
            quantity_resolution: None,
            source_resolution: None,
        })
        .collect();
    let tax = field("tax");
    // No automatic claim about whether printed tax is already included.
    let tax_basis = if tax.is_empty() || extraction::parse_money(&tax) == Ok(0) {
        TaxBasis::Included
    } else {
        TaxBasis::Unknown
    };
    let po = field("po_number");
    Review {
        vendor_id,
        invoice_number: field("invoice_number"),
        invoice_date: field("invoice_date").parse().ok(),
        due_date: field("due_date").parse().ok(),
        total: field("total"),
        currency: field("currency"),
        memo: if po.is_empty() {
            String::new()
        } else {
            format!("PO {po}")
        },
        rows,
        tax,
        tax_basis,
        source_checked: false,
        coverage_resolution: None,
    }
}
pub async fn get(cx: &mut Cx<'_>, input: Get) -> Result<Value, CommandError> {
    store::get_document(&mut cx.tx, input.document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    let history = store::invoice_review_history(&mut cx.tx, input.document_id).await?;
    let extractions = store::document_extractions(&mut cx.tx, input.document_id).await?;
    let current_version = documents::current_arrangement(cx, input.document_id)
        .await?
        .map_or(0, |a| a.version);
    let latest = extractions.iter().find(|e| {
        e.status == "read"
            && e.arrangement_version == current_version
            && e.analysis.as_ref().is_none_or(|a| a["stale"] != true)
    });
    let enabled = store::invoice_review_enabled(&mut cx.tx).await?;
    let required = enabled || store::invoice_review_enrolled(&mut cx.tx, input.document_id).await?;
    let version = documents::current_arrangement(cx, input.document_id)
        .await?
        .map_or(0, |a| a.version);
    let seed = initial(latest);
    let payload = history.first().map(|r| &r.payload).unwrap_or(&seed);
    let ex = history
        .first()
        .map(|r| r.extraction_id)
        .unwrap_or_else(|| latest.map(|e| e.id));
    let (validation, analysis) = validate(cx, input.document_id, payload, ex).await?;
    Ok(
        json!({"analysis":analysis,"enabled":enabled,"required":required,"review":history.first(),"history":history,"initial":seed,"extraction_id":latest.map(|e|e.id),"arrangement_version":version,"validation":validation}),
    )
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub document_id: Uuid,
    pub extraction_id: Option<Uuid>,
    pub expected_version: i32,
    pub arrangement_version: i32,
    pub request_id: Uuid,
    pub payload: Review,
}
async fn check_source(
    cx: &mut Cx<'_>,
    input: &Save,
    old: Option<&InvoiceReview>,
) -> Result<(), CommandError> {
    rules::check_shape(&input.payload).map_err(CommandError::Invalid)?;
    if let Some(v) = &input.payload.vendor_id {
        let id = v
            .parse()
            .map_err(|_| CommandError::Invalid("Invalid vendor ID".into()))?;
        store::get_vendor(&mut cx.tx, id)
            .await?
            .ok_or(CommandError::NotFound("vendor"))?;
    }
    if let Some(id) = input.extraction_id {
        let ex = store::get_extraction(&mut cx.tx, id)
            .await?
            .ok_or(CommandError::NotFound("extraction"))?;
        if ex.document_id != input.document_id || ex.status != "read" {
            return Err(CommandError::Invalid(
                "Extraction does not belong to this document or failed".into(),
            ));
        }
        if ex.arrangement_version != input.arrangement_version {
            return Err(CommandError::Conflict(
                "Read the current pages again before saving this review".into(),
            ));
        }
        let count = ex
            .answer
            .as_ref()
            .and_then(|v| v["lines"].as_array())
            .map_or(0, Vec::len);
        let mut indices: Vec<_> = input
            .payload
            .rows
            .iter()
            .filter_map(|r| r.source_index)
            .collect();
        indices.sort_unstable();
        if indices != (0..count).collect::<Vec<_>>() {
            return Err(CommandError::Invalid(
                "Keep every extracted row; exclude it with a reason instead of removing it".into(),
            ));
        }
    } else if input.payload.rows.iter().any(|r| r.source_index.is_some()) {
        return Err(CommandError::Invalid(
            "Source rows need an extraction".into(),
        ));
    }
    if let Some(old) = old {
        // Re-reading is an explicit new lineage. Within one reading, identities
        // and excluded rows survive every revision.
        if old.extraction_id == input.extraction_id {
            for row in &old.payload.rows {
                if !input
                    .payload
                    .rows
                    .iter()
                    .any(|r| r.id == row.id && r.source_index == row.source_index)
                {
                    return Err(CommandError::Invalid(
                        "Keep prior rows and their source IDs; exclude with a reason".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}
async fn writable(
    cx: &mut Cx<'_>,
    doc: Uuid,
    expected: i32,
    arrangement: i32,
) -> Result<Vec<InvoiceReview>, CommandError> {
    let d = store::get_document(&mut cx.tx, doc)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    if d.status != "inbox" {
        return Err(CommandError::Conflict(
            "Only inbox documents can be reviewed".into(),
        ));
    }
    if !store::invoice_review_enabled(&mut cx.tx).await?
        && !store::invoice_review_enrolled(&mut cx.tx, doc).await?
    {
        return Err(CommandError::Conflict(
            "Enable line review in Company settings first".into(),
        ));
    }
    let history = store::invoice_review_history(&mut cx.tx, doc).await?;
    if history.first().map_or(0, |r| r.version) != expected {
        return Err(CommandError::Conflict(
            "Review changed. Reload before editing.".into(),
        ));
    }
    if documents::current_arrangement(cx, doc)
        .await?
        .map_or(0, |a| a.version)
        != arrangement
    {
        return Err(CommandError::Conflict(
            "Pages changed. Reload and review the current pages.".into(),
        ));
    }
    Ok(history)
}
fn invalidate_resolutions(input: &mut Save, old: Option<&InvoiceReview>) {
    if let Some(old) = old {
        for row in &mut input.payload.rows {
            if let Some(before) = old.payload.rows.iter().find(|r| r.id == row.id) {
                if (row.amount != before.amount
                    || row.description != before.description
                    || row.details != before.details)
                    && row.quantity_resolution == before.quantity_resolution
                {
                    row.quantity_resolution = None;
                }
                if (row.amount != before.amount
                    || row.description != before.description
                    || row.details != before.details)
                    && row.source_resolution == before.source_resolution
                {
                    row.source_resolution = None;
                }
            }
        }
        if (old.arrangement_version != input.arrangement_version
            || old.extraction_id != input.extraction_id)
            && old.payload.coverage_resolution == input.payload.coverage_resolution
        {
            input.payload.coverage_resolution = None;
        }
    }
}
pub async fn save(cx: &mut Cx<'_>, mut input: Save) -> Result<Value, CommandError> {
    store::lock_invoice_settings(&mut cx.tx, cx.org).await?;
    let request =
        json!({"command":"document.review_save","actor_kind":cx.actor.kind,"input":input});
    if let Some(r) =
        store::invoice_review_replay(&mut cx.tx, &cx.actor.name, input.request_id).await?
    {
        if r.request_payload != request {
            return Err(CommandError::Conflict(
                "Request ID was already used for different input".into(),
            ));
        }
        return result(cx, &r).await;
    }
    let history = writable(
        cx,
        input.document_id,
        input.expected_version,
        input.arrangement_version,
    )
    .await?;
    invalidate_resolutions(&mut input, history.first());
    check_source(cx, &input, history.first()).await?;
    if history.first().is_some_and(|r| {
        r.payload == input.payload
            && r.extraction_id == input.extraction_id
            && r.arrangement_version == input.arrangement_version
            && r.summary_reason.is_none()
    }) {
        return Err(CommandError::Conflict("Review is unchanged".into()));
    }
    let r = store::insert_invoice_review(
        &mut cx.tx,
        cx.org,
        store::NewInvoiceReview {
            document_id: input.document_id,
            extraction_id: input.extraction_id,
            version: input.expected_version + 1,
            arrangement_version: input.arrangement_version,
            payload: &input.payload,
            summary_reason: None,
            actor: &cx.actor.name,
            now: cx.now,
            request_id: input.request_id,
            request_payload: &request,
        },
    )
    .await?;
    cx.emit("document.review_saved",input.document_id,json!({"review_id":r.id,"version":r.version,"previous_review_id":history.first().map(|r|r.id),"summary_invalidated":history.first().is_some_and(|r|r.summary_reason.is_some())})).await?;
    result(cx, &r).await
}
pub async fn preview(cx: &mut Cx<'_>, mut input: Save) -> Result<Value, CommandError> {
    let history = writable(
        cx,
        input.document_id,
        input.expected_version,
        input.arrangement_version,
    )
    .await?;
    invalidate_resolutions(&mut input, history.first());
    check_source(cx, &input, history.first()).await?;
    let (validation, analysis) =
        validate(cx, input.document_id, &input.payload, input.extraction_id).await?;
    Ok(json!({"validation":validation,"analysis":analysis}))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptSummary {
    document_id: Uuid,
    expected_version: i32,
    arrangement_version: i32,
    request_id: Uuid,
    reason: String,
}
pub async fn accept_summary(cx: &mut Cx<'_>, input: AcceptSummary) -> Result<Value, CommandError> {
    store::lock_invoice_settings(&mut cx.tx, cx.org).await?;
    let request = json!({"command":"document.review_accept_summary","actor_kind":cx.actor.kind,"input":input});
    if let Some(r) =
        store::invoice_review_replay(&mut cx.tx, &cx.actor.name, input.request_id).await?
    {
        if r.request_payload != request {
            return Err(CommandError::Conflict("Request ID was reused".into()));
        }
        return result(cx, &r).await;
    }
    let history = writable(
        cx,
        input.document_id,
        input.expected_version,
        input.arrangement_version,
    )
    .await?;
    let old = history
        .first()
        .ok_or(CommandError::Invalid("Save the review first".into()))?;
    if input.reason.trim().is_empty() || input.reason.len() > 1000 {
        return Err(CommandError::Invalid(
            "Give a summary-only reason (1–1000 characters)".into(),
        ));
    }
    if !validate(cx, old.document_id, &old.payload, old.extraction_id)
        .await?
        .0
        .header_ready
    {
        return Err(CommandError::Invalid(
            "Review invoice details and source before accepting summary-only".into(),
        ));
    }
    if old.summary_reason.is_some() {
        return Err(CommandError::Conflict(
            "Summary-only was already accepted".into(),
        ));
    }
    let r = store::insert_invoice_review(
        &mut cx.tx,
        cx.org,
        store::NewInvoiceReview {
            document_id: input.document_id,
            extraction_id: old.extraction_id,
            version: old.version + 1,
            arrangement_version: old.arrangement_version,
            payload: &old.payload,
            summary_reason: Some(input.reason.trim()),
            actor: &cx.actor.name,
            now: cx.now,
            request_id: input.request_id,
            request_payload: &request,
        },
    )
    .await?;
    cx.emit(
        "document.review_summary_accepted",
        input.document_id,
        json!({"review_id":r.id,"previous_review_id":old.id,"reason":input.reason}),
    )
    .await?;
    result(cx, &r).await
}

/// Called under the document lock by bill.create, even if the client omitted
/// extraction/review IDs. Settings off never unenrolls an existing review.
pub async fn for_bill(
    cx: &mut Cx<'_>,
    doc: Uuid,
    review_id: Option<Uuid>,
) -> Result<Option<InvoiceReview>, CommandError> {
    store::get_document(&mut cx.tx, doc)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    let history = store::invoice_review_history(&mut cx.tx, doc).await?;
    let required = store::invoice_review_enabled(&mut cx.tx).await?
        || store::invoice_review_enrolled(&mut cx.tx, doc).await?;
    if !required && review_id.is_none() {
        return Ok(None);
    }
    let r = history
        .first()
        .filter(|r| Some(r.id) == review_id)
        .ok_or(CommandError::Conflict(
            "Save and supply the current invoice review before creating a bill".into(),
        ))?;
    if documents::current_arrangement(cx, doc)
        .await?
        .map_or(0, |a| a.version)
        != r.arrangement_version
    {
        return Err(CommandError::Conflict("Review is of older pages".into()));
    }
    if let Some(id) = r.extraction_id {
        let ex = store::get_extraction(&mut cx.tx, id)
            .await?
            .ok_or(CommandError::NotFound("extraction"))?;
        if ex
            .checks
            .as_ref()
            .and_then(|c| c.as_array())
            .is_some_and(|checks| {
                checks
                    .iter()
                    .any(|c| c["check"] == "one_invoice" && c["ok"] == false)
            })
        {
            return Err(CommandError::Invalid(
                "Split the invoices into separate documents before creating a reviewed bill".into(),
            ));
        }
    }
    let validation = validate(cx, r.document_id, &r.payload, r.extraction_id)
        .await?
        .0;
    if !validation.header_ready || (!validation.ready && r.summary_reason.is_none()) {
        return Err(CommandError::Invalid(
            "Resolve line review issues or have a person explicitly accept a summary-only bill"
                .into(),
        ));
    }
    Ok(Some(r.clone()))
}
