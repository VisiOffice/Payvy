//! The inbox: original files waiting to become bills.

use base64::Engine;
use payvy_domain::bills::State;
use payvy_domain::documents::{self as rules};
use payvy_domain::pages::{self, PageRef};
use payvy_store::{self as store, Arrangement, NewDocument};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{CommandError, Cx, text};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upload {
    filename: String,
    content_base64: String,
}

pub async fn upload(cx: &mut Cx<'_>, input: Upload) -> Result<Value, CommandError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input.content_base64.trim())
        .map_err(|e| CommandError::Invalid(format!("content_base64 isn't valid base64: {e}")))?;
    let kind = rules::check(&bytes).map_err(|e| CommandError::Invalid(e.to_string()))?;
    let filename = rules::clean_filename(&input.filename);
    match receive(cx, bytes, kind, &filename, None).await? {
        Received::New(doc) => Ok(json!(doc)),
        Received::AlreadyHere(existing) => Err(already_here(&existing)),
    }
}

pub(super) enum Received {
    New(store::Document),
    AlreadyHere(store::Document),
}

/// Stores a checked file and puts it in the inbox, unless this organization
/// already has it. `email_id` is set for an email attachment.
pub(super) async fn receive(
    cx: &mut Cx<'_>,
    bytes: Vec<u8>,
    kind: rules::Kind,
    filename: &str,
    email_id: Option<Uuid>,
) -> Result<Received, CommandError> {
    let storage = cx.storage()?;
    let sha = hex::encode(Sha256::digest(&bytes));
    if let Some(existing) = store::document_by_hash(&mut cx.tx, &sha).await? {
        return Ok(Received::AlreadyHere(existing));
    }
    let key = rules::storage_key(&cx.org.to_string(), &sha, kind);
    // The object is written before the row: a failed transaction leaves an
    // object nothing points at, and a retry writes the same key again.
    let size_bytes = bytes.len() as i64;
    let page_count = crate::page_count::count(bytes.clone(), kind)
        .await
        .map_or(0, |n| n as i32);
    if !cx.preview {
        storage.put(&key, bytes, kind.content_type()).await?;
    }
    let source = if email_id.is_some() {
        "email"
    } else {
        "upload"
    };
    let new = NewDocument {
        sha256: &sha,
        content_type: kind.content_type(),
        size_bytes,
        filename,
        storage_key: &key,
        source,
        email_id,
        received_at: cx.now,
        created_by: &cx.actor.name,
        page_count: Some(page_count),
    };
    let Some(doc) = store::insert_document(&mut cx.tx, cx.org, &new).await? else {
        let existing = store::document_by_hash(&mut cx.tx, &sha)
            .await?
            .ok_or_else(|| anyhow::anyhow!("document {sha} conflicted but can't be read"))?;
        return Ok(Received::AlreadyHere(existing));
    };
    cx.emit(
        "document.received",
        doc.id,
        json!({ "document_id": doc.id, "filename": doc.filename, "source": source, "email_id": email_id, "size_bytes": doc.size_bytes }),
    )
    .await?;
    Ok(Received::New(doc))
}

fn already_here(existing: &store::Document) -> CommandError {
    CommandError::Conflict(format!(
        "this exact file is already here: {}",
        where_is(existing)
    ))
}

/// "document X (name.pdf), filed with bill Y"
pub(super) fn where_is(existing: &store::Document) -> String {
    let place = match (existing.status.as_str(), existing.bill_id) {
        ("filed", Some(bill)) => format!("filed with bill {bill}"),
        (status, _) => format!("in {status}"),
    };
    format!("document {} ({}), {place}", existing.id, existing.filename)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    status: Option<String>,
    limit: Option<i64>,
}

pub async fn list(cx: &mut Cx<'_>, input: List) -> Result<Value, CommandError> {
    let status = input.status.unwrap_or_else(|| "inbox".into());
    if !["inbox", "filed", "trashed"].contains(&status.as_str()) {
        return Err(CommandError::Invalid(
            "status is inbox, filed or trashed".into(),
        ));
    }
    let limit = input.limit.unwrap_or(100).clamp(1, 500);
    Ok(json!({ "documents": store::list_documents(&mut cx.tx, &status, limit).await? }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Get {
    document_id: Uuid,
}

pub async fn get(cx: &mut Cx<'_>, input: Get) -> Result<Value, CommandError> {
    let doc = store::get_document(&mut cx.tx, input.document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    detail(cx, doc).await
}

/// The document, its pages, every reading and its history.
async fn detail(cx: &mut Cx<'_>, doc: store::Document) -> Result<Value, CommandError> {
    let url = cx.storage()?.download_url(&doc.storage_key);
    let count = page_count(cx, &doc).await?;
    let versions = store::document_arrangements(&mut cx.tx, doc.id).await?;
    let version = versions.first().map_or(0, |a| a.version);
    let locked = lock(cx, &doc).await?;
    let extractions: Vec<Value> = store::document_extractions(&mut cx.tx, doc.id)
        .await?
        .iter()
        .map(|row| {
            let mut summary = super::extract::summary(row);
            summary["out_of_date"] = json!(row.arrangement_version < version);
            summary
        })
        .collect();
    let history = store::list_events(&mut cx.tx, Some(doc.id), 200).await?;
    let pages = json!({
        "count": count,
        "version": version,
        "arrangement_id": versions.first().map(|a| a.id),
        "arrangement": arranged(versions.first(), count),
        "locked": locked.is_some(),
        "locked_reason": locked,
        "versions": versions,
    });
    Ok(
        json!({ "document": doc, "download_url": url, "download_url_expires_in_seconds": 900,
               "pages": pages, "extractions": extractions, "history": history }),
    )
}

/// The pages as an arrangement shows them; the original's when there is
/// none. None when the page count isn't known.
pub(super) fn arranged(current: Option<&Arrangement>, count: Option<u32>) -> Option<Vec<PageRef>> {
    match current {
        Some(a) => Some(a.pages.clone()),
        None => count.map(pages::original),
    }
}

/// How many pages the original has: counted at upload, or now for a
/// document from before page counts (then kept). None when the file can't
/// be read as a PDF, or can't be fetched right now.
pub(super) async fn page_count(
    cx: &mut Cx<'_>,
    doc: &store::Document,
) -> Result<Option<u32>, CommandError> {
    if let Some(n) = doc.page_count {
        return Ok(u32::try_from(n).ok().filter(|n| *n > 0));
    }
    let Ok(storage) = cx.storage() else {
        return Ok(None);
    };
    let bytes = match storage.get(&doc.storage_key).await {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(document = %doc.id, "couldn't fetch the file to count its pages: {e:#}");
            return Ok(None);
        }
    };
    let Ok(kind) = rules::check(&bytes) else {
        return Ok(None);
    };
    let count = crate::page_count::count(bytes, kind).await;
    store::set_page_count(&mut cx.tx, doc.id, count.map_or(0, |n| n as i32)).await?;
    Ok(count)
}

/// Why this document's pages can't change, if they can't. A filed
/// document's pages lock once its bill has any approval or is no longer
/// open: approvers approved what they saw.
async fn lock(cx: &mut Cx<'_>, doc: &store::Document) -> Result<Option<String>, CommandError> {
    if doc.status == "trashed" {
        return Ok(Some(
            "This document was set aside as not a bill, so its pages can't change".into(),
        ));
    }
    let Some(bill_id) = doc.bill_id else {
        return Ok(None);
    };
    let bill = store::get_bill(&mut cx.tx, bill_id).await?.ok_or_else(|| {
        anyhow::anyhow!("document {} is filed with missing bill {bill_id}", doc.id)
    })?;
    let (approvals, _) = store::facts(&mut cx.tx, &[bill.id]).await?;
    let approved = !approvals.is_empty() || bill.released_at.is_some() || bill.state == State::Paid;
    Ok(if approved {
        Some("Pages are locked after approval".into())
    } else if bill.state != State::Open {
        Some(format!(
            "Pages are locked: the bill is {}",
            bill.state.as_str()
        ))
    } else {
        None
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arrange {
    document_id: Uuid,
    expected_version: i32,
    pages: Vec<PageRef>,
}

/// Sets the order, rotation and hidden pages of a document in one change,
/// as a new version of its arrangement. The file itself never changes.
pub async fn arrange(cx: &mut Cx<'_>, input: Arrange) -> Result<Value, CommandError> {
    let doc = store::get_document(&mut cx.tx, input.document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    if let Some(reason) = lock(cx, &doc).await? {
        return Err(CommandError::Conflict(reason));
    }
    let count = page_count(cx, &doc).await?.ok_or_else(|| {
        CommandError::Conflict(
            "this file can't be read as a PDF, so its pages can't be arranged".into(),
        )
    })?;
    let versions = store::document_arrangements(&mut cx.tx, doc.id).await?;
    let current = versions.first();
    let version = current.map_or(0, |a| a.version);
    if input.expected_version != version {
        let by = current.map_or("", |a| a.created_by.as_str());
        return Err(CommandError::Conflict(format!(
            "the pages changed since version {}: {by} made version {version}. \
             Get the document again and make your change to that.",
            input.expected_version
        )));
    }
    pages::validate(&input.pages, count).map_err(|e| CommandError::Invalid(e.to_string()))?;
    let old = arranged(current, Some(count)).unwrap_or_default();
    let changes = pages::changes(&old, &input.pages);
    if changes.is_empty() {
        return Err(CommandError::Conflict(
            "the pages are already arranged this way".into(),
        ));
    }
    let summary = pages::describe(&changes);
    let row = store::insert_arrangement(
        &mut cx.tx,
        cx.org,
        doc.id,
        version + 1,
        &input.pages,
        cx.now,
        &cx.actor.name,
    )
    .await?;
    cx.emit(
        "document.arranged",
        doc.id,
        json!({ "document_id": doc.id, "arrangement_id": row.id, "version": row.version,
                "previous_version": version, "changes": changes, "summary": summary }),
    )
    .await?;
    // Not locked, so a bill it is filed with is open and unapproved: it
    // moves to the new version, on its own record.
    if let Some(bill_id) = doc.bill_id {
        store::pin_arrangement(&mut cx.tx, bill_id, row.id).await?;
        cx.emit(
            "bill.pages_arranged",
            bill_id,
            json!({ "bill_id": bill_id, "document_id": doc.id, "arrangement_id": row.id,
                    "version": row.version, "changes": changes, "summary": summary }),
        )
        .await?;
    }
    let doc = store::get_document(&mut cx.tx, doc.id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    detail(cx, doc).await
}

/// The current arrangement of a document, for a bill to pin. None while it
/// is the original.
pub(super) async fn current_arrangement(
    cx: &mut Cx<'_>,
    document_id: Uuid,
) -> Result<Option<Arrangement>, CommandError> {
    Ok(store::document_arrangements(&mut cx.tx, document_id)
        .await?
        .into_iter()
        .next())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trash {
    document_id: Uuid,
    reason: String,
}

pub async fn trash(cx: &mut Cx<'_>, input: Trash) -> Result<Value, CommandError> {
    let reason = text(Some(input.reason))
        .ok_or_else(|| CommandError::Invalid("reason can't be empty".into()))?;
    let doc = store::get_document(&mut cx.tx, input.document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    if doc.status != "inbox" {
        return Err(CommandError::Conflict(format!(
            "this document is {}, not in the inbox",
            doc.status
        )));
    }
    store::trash_document(&mut cx.tx, doc.id, &reason).await?;
    cx.emit(
        "document.trashed",
        doc.id,
        json!({ "document_id": doc.id, "reason": reason }),
    )
    .await?;
    let doc = store::get_document(&mut cx.tx, doc.id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    Ok(json!(doc))
}

/// Files an inbox document with a new bill. Called by `bill.create`.
pub(super) async fn file_with_bill(
    cx: &mut Cx<'_>,
    document_id: Uuid,
    bill_id: Uuid,
) -> Result<(), CommandError> {
    let doc = store::get_document(&mut cx.tx, document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    if doc.status != "inbox" {
        return Err(CommandError::Conflict(match doc.bill_id {
            Some(b) => format!("document {} is already filed with bill {b}", doc.id),
            None => format!("document {} is {}, not in the inbox", doc.id, doc.status),
        }));
    }
    store::file_document(&mut cx.tx, doc.id, bill_id).await?;
    cx.emit(
        "document.filed",
        doc.id,
        json!({ "document_id": doc.id, "bill_id": bill_id }),
    )
    .await?;
    Ok(())
}

/// The arrangement a bill pinned, as `bill.get` shows it: its version and
/// pages (None while the page count isn't known).
pub(super) async fn pinned(
    cx: &mut Cx<'_>,
    doc: &store::Document,
    arrangement_id: Option<Uuid>,
) -> Result<Value, CommandError> {
    let count = page_count(cx, doc).await?;
    let row = match arrangement_id {
        Some(id) => Some(
            store::get_arrangement(&mut cx.tx, id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("bill points at missing arrangement {id}"))?,
        ),
        None => None,
    };
    Ok(json!({
        "document_id": doc.id,
        "arrangement_id": arrangement_id,
        "version": row.as_ref().map_or(0, |a| a.version),
        "count": count,
        "pages": arranged(row.as_ref(), count),
    }))
}
