//! `document.submit_reading`: an outside agent's reading of an inbox document,
//! checked exactly like Payvy's own (`extract::check_reading`) and kept as an
//! extraction. This is how a company reads invoices with its own agent and
//! model, as on a plan without Payvy's reader: Payvy makes no model call and
//! records no AI usage, so no reader entitlement is needed. The agent reads
//! the file (document.get has a download link) and reports only what is
//! printed, in the same schema the models fill, amounts as strings. Payvy
//! converts, checks, matches the vendor and finds duplicates, so a weak
//! reading produces a draft that fails checks, never a wrong bill. As with
//! document.extract, nothing becomes a bill until bill.create quotes it.

use payvy_domain::extraction::{self as rules, Read};
use payvy_store::{self as store, ExtractionRead, NewExtraction};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::extract::{Checked, auto_split, check_reading, summary};
use super::{CommandError, Cx};

/// The provider recorded on an agent's reading, beside the model it names.
pub const AGENT_PROVIDER: &str = "agent";

/// A reading is a page of text, not a file; anything larger is a mistake.
const MAX_READING_BYTES: usize = 256 * 1024;
const MAX_LINES: usize = 1000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitReading {
    document_id: Uuid,
    reading: Read,
    /// The model the agent used, for the record ("gpt-5.1", say).
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    again: bool,
}

pub async fn submit(cx: &mut Cx<'_>, input: SubmitReading) -> Result<Value, CommandError> {
    let SubmitReading {
        document_id,
        reading,
        model,
        again,
    } = input;
    let size = serde_json::to_vec(&reading).map_or(usize::MAX, |b| b.len());
    if size > MAX_READING_BYTES || reading.lines.len() > MAX_LINES {
        return Err(CommandError::Invalid(format!(
            "a reading is at most {} KB and {MAX_LINES} lines",
            MAX_READING_BYTES / 1024
        )));
    }
    let model = match model.as_deref().map(str::trim) {
        None | Some("") => "unspecified".to_owned(),
        Some(m) if m.chars().count() > 80 || m.chars().any(char::is_control) => {
            return Err(CommandError::Invalid(
                "model is a name of at most 80 characters".into(),
            ));
        }
        Some(m) => m.to_owned(),
    };

    // The same order and refusals as document.extract.
    store::lock_invoice_settings(&mut cx.tx, cx.org).await?;
    let doc = store::get_document(&mut cx.tx, document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    if doc.status != "inbox" {
        return Err(CommandError::Conflict(format!(
            "this document is {}, not in the inbox",
            doc.status
        )));
    }
    let previous = store::document_extractions(&mut cx.tx, doc.id).await?;
    if !again && let Some(done) = previous.iter().find(|e| e.status == "read") {
        return Err(CommandError::Conflict(format!(
            "this document was already read by {} (extraction {}); pass again: true to submit another reading",
            done.model, done.id
        )));
    }
    let arrangement_version = super::documents::current_arrangement(cx, doc.id)
        .await?
        .map_or(0, |a| a.version);
    let (doc_pages, _) = super::documents::pages_to_read(cx, &doc).await?;

    let Checked {
        draft,
        checks,
        matched,
        invoices,
    } = check_reading(cx, &doc, &reading, &doc_pages, Vec::new()).await?;
    let invoices_json = (!invoices.is_empty()).then(|| json!(invoices));
    let answer = json!(reading);
    let mut row = store::insert_extraction(
        &mut cx.tx,
        cx.org,
        &NewExtraction {
            document_id: doc.id,
            provider: AGENT_PROVIDER,
            model: &model,
            outcome: Ok(ExtractionRead {
                answer: &answer,
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
    // Schema version 1: the as-printed schema. The hash says which one the
    // agent was given, so a later schema change is visible on old readings.
    let analysis = json!({
        "schema_version": 1,
        "source": AGENT_PROVIDER,
        "schema_hash": hex::encode(Sha256::digest(serde_json::to_vec(&rules::schema()).unwrap_or_default())),
        "source_sha256": doc.sha256,
        "selected_pages": doc_pages,
        "arrangement_version": arrangement_version,
        "stale": false,
    });
    store::set_extraction_analysis(&mut cx.tx, row.id, &analysis).await?;
    row.analysis = Some(analysis);
    let ready = checks.iter().all(|c| c.ok);
    cx.emit(
        "document.extracted",
        doc.id,
        json!({ "document_id": doc.id, "extraction_id": row.id, "provider": AGENT_PROVIDER,
                "model": row.model, "ready": ready, "invoices": invoices.len().max(1) }),
    )
    .await?;
    let mut out = summary(&row);
    auto_split(cx, &doc, arrangement_version, &invoices, false, &mut out).await?;
    Ok(out)
}
