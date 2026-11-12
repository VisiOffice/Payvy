//! "Create N bills" on a file of several invoices. The command splits the
//! file at once and records a batch, one part per invoice; the batch worker
//! (`crate::bill_batch_worker`) then reads each part and makes its bill.

use std::collections::BTreeSet;

use payvy_store as store;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::documents::{arranged, current_arrangement, page_count, split_pages};
use super::{CommandError, Cx};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBills {
    document_id: Uuid,
    expected_version: i32,
    request_id: Uuid,
    groups: Vec<Vec<u32>>,
    #[serde(default)]
    leftover: Option<Vec<u32>>,
}

/// At most this many invoices in one request.
const MAX_PARTS: usize = 200;

pub async fn create(cx: &mut Cx<'_>, input: CreateBills) -> Result<Value, CommandError> {
    // The document is locked first, so a double click waits for the first
    // request and then finds its batch below.
    let doc = store::get_document(&mut cx.tx, input.document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    if let Some(batch) = store::bill_batch_by_request(&mut cx.tx, input.request_id).await? {
        if batch.document_id != doc.id {
            return Err(CommandError::Invalid(
                "request_id was already used for another document; send a new one".into(),
            ));
        }
        let mut out = batch_json(cx, &batch).await?;
        out["repeated"] = json!(true);
        return Ok(out);
    }
    if doc.status != "inbox" {
        return Err(CommandError::Conflict(format!(
            "this document is {}, not in To review, so its invoices can't be made bills from here",
            doc.status
        )));
    }
    if input.groups.is_empty() || input.groups.len() > MAX_PARTS {
        return Err(CommandError::Invalid(format!(
            "groups must hold between 1 and {MAX_PARTS} invoices"
        )));
    }
    // groups and leftover, when given, must account for every visible page
    // once: the person and the server agree on what stays behind.
    if let Some(leftover) = &input.leftover {
        let count = page_count(cx, &doc).await?;
        let current = current_arrangement(cx, doc.id).await?;
        let visible: BTreeSet<u32> = arranged(current.as_ref(), count)
            .unwrap_or_default()
            .iter()
            .filter(|p| !p.hidden)
            .map(|p| p.page)
            .collect();
        let named: Vec<u32> = input
            .groups
            .iter()
            .flatten()
            .chain(leftover)
            .copied()
            .collect();
        let set: BTreeSet<u32> = named.iter().copied().collect();
        if set.len() != named.len() || set != visible {
            return Err(CommandError::Invalid(
                "groups and leftover must name every visible page of this document exactly once"
                    .into(),
            ));
        }
    }

    // What the reader found for each group, where the pages are the same.
    let read = store::latest_extractions(&mut cx.tx, &[doc.id]).await?;
    let found: Vec<(Vec<u32>, Option<String>, Option<i64>)> = read
        .first()
        .and_then(|x| x.invoices.as_ref())
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .map(|g| {
            let pages = g["pages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p.as_u64().map(|p| p as u32))
                .collect();
            (
                pages,
                g["invoice_number"].as_str().map(str::to_owned),
                g["total_cents"].as_i64(),
            )
        })
        .collect();

    let parts = split_pages(cx, &doc, Some(input.expected_version), &input.groups).await?;
    let batch = store::insert_bill_batch(
        &mut cx.tx,
        cx.org,
        doc.id,
        input.request_id,
        &cx.actor.name,
        cx.now,
    )
    .await?;
    for (i, (part, pages)) in parts.iter().zip(&input.groups).enumerate() {
        let label = found.iter().find(|(p, _, _)| p == pages);
        let numbers: Vec<i32> = pages.iter().map(|p| *p as i32).collect();
        store::insert_bill_batch_part(
            &mut cx.tx,
            cx.org,
            batch.id,
            i as i32,
            part.id,
            &numbers,
            label.and_then(|(_, n, _)| n.as_deref()),
            label.and_then(|(_, _, t)| *t),
            cx.now,
        )
        .await?;
    }
    cx.emit(
        "document.bill_batch",
        doc.id,
        json!({ "batch_id": batch.id, "invoices": parts.len(),
                "summary": format!("asked for {} bills, one per invoice", parts.len()) }),
    )
    .await?;
    batch_json(cx, &batch).await
}

/// A batch and each part's progress, for document.get and the command's answer.
pub(super) async fn batch_json(
    cx: &mut Cx<'_>,
    batch: &store::BillBatch,
) -> Result<Value, CommandError> {
    let parts = store::bill_batch_parts(&mut cx.tx, batch.id).await?;
    let ids: Vec<Uuid> = parts.iter().map(|p| p.document_id).collect();
    let names: std::collections::HashMap<Uuid, String> =
        store::document_filenames(&mut cx.tx, &ids)
            .await?
            .into_iter()
            .collect();
    let count = |s: &str| parts.iter().filter(|p| p.state == s).count();
    let counts = json!({
        "reading": count("reading"), "created": count("created"), "stacked": count("stacked"),
        "needs_you": count("needs_you"), "withdrawn": count("withdrawn"),
    });
    let parts: Vec<Value> = parts
        .iter()
        .map(|p| {
            json!({
                "position": p.position, "document_id": p.document_id,
                "filename": names.get(&p.document_id), "pages": p.pages,
                "invoice_number": p.invoice_number, "total_cents": p.total_cents,
                "state": p.state, "bill_id": p.bill_id, "note": p.note,
            })
        })
        .collect();
    Ok(json!({
        "id": batch.id, "document_id": batch.document_id, "request_id": batch.request_id,
        "requested_by": batch.requested_by, "requested_at": batch.requested_at,
        "done": parts.iter().all(|p| p["state"] != "reading"),
        "counts": counts, "parts": parts,
    }))
}

/// The newest batch made from this document, if any.
pub(super) async fn latest(cx: &mut Cx<'_>, document_id: Uuid) -> Result<Value, CommandError> {
    match store::latest_bill_batch(&mut cx.tx, document_id).await? {
        Some(b) => batch_json(cx, &b).await,
        None => Ok(Value::Null),
    }
}
