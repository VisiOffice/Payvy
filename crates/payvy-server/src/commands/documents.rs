//! The inbox: original files waiting to become bills.

use base64::Engine;
use payvy_domain::documents::{self as rules};
use payvy_store::{self as store, NewDocument};
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
    let storage = cx.storage()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input.content_base64.trim())
        .map_err(|e| CommandError::Invalid(format!("content_base64 isn't valid base64: {e}")))?;
    let kind = rules::check(&bytes).map_err(|e| CommandError::Invalid(e.to_string()))?;
    let sha = hex::encode(Sha256::digest(&bytes));
    if let Some(existing) = store::document_by_hash(&mut cx.tx, &sha).await? {
        return Err(already_here(&existing));
    }
    let key = rules::storage_key(&cx.org.to_string(), &sha, kind);
    // The object is written before the row: a failed transaction leaves an
    // object nothing points at, and a retry writes the same key again.
    if !cx.preview {
        storage
            .put(&key, bytes.clone(), kind.content_type())
            .await?;
    }
    let filename = rules::clean_filename(&input.filename);
    let new = NewDocument {
        sha256: &sha,
        content_type: kind.content_type(),
        size_bytes: bytes.len() as i64,
        filename: &filename,
        storage_key: &key,
        source: "upload",
        received_at: cx.now,
        created_by: &cx.actor.name,
    };
    let doc = match store::insert_document(&mut cx.tx, cx.org, &new).await? {
        Some(d) => d,
        None => {
            let existing = store::document_by_hash(&mut cx.tx, &sha)
                .await?
                .ok_or_else(|| anyhow::anyhow!("document {sha} conflicted but can't be read"))?;
            return Err(already_here(&existing));
        }
    };
    cx.emit(
        "document.received",
        doc.id,
        json!({ "document_id": doc.id, "filename": doc.filename, "source": "upload", "size_bytes": doc.size_bytes }),
    )
    .await?;
    Ok(json!(doc))
}

fn already_here(existing: &store::Document) -> CommandError {
    let place = match (existing.status.as_str(), existing.bill_id) {
        ("filed", Some(bill)) => format!("filed with bill {bill}"),
        (status, _) => format!("in {status}"),
    };
    CommandError::Conflict(format!(
        "this exact file is already here: document {} ({}), {place}",
        existing.id, existing.filename
    ))
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
    let url = cx.storage()?.download_url(&doc.storage_key);
    Ok(json!({ "document": doc, "download_url": url, "download_url_expires_in_seconds": 900 }))
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
