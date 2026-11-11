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
        ("split", _) => "split into other documents".to_owned(),
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
    if !["inbox", "filed", "trashed", "split"].contains(&status.as_str()) {
        return Err(CommandError::Invalid(
            "status is inbox, filed, trashed or split".into(),
        ));
    }
    let limit = input.limit.unwrap_or(100).clamp(1, 500);
    let docs = store::list_documents(&mut cx.tx, &status, limit).await?;
    let sources: Vec<Uuid> = docs.iter().filter_map(|d| d.derived_from).collect();
    let names = store::document_filenames(&mut cx.tx, &sources).await?;
    let documents: Vec<Value> = docs
        .iter()
        .map(|d| {
            let mut v = json!(d);
            v["split_from"] = split_from(d, &names);
            v
        })
        .collect();
    Ok(json!({ "documents": documents }))
}

/// `{id, filename}` of the document this one was split from, or null.
fn split_from(doc: &store::Document, names: &[(Uuid, String)]) -> Value {
    match doc.derived_from {
        Some(id) => json!({
            "id": id,
            "filename": names.iter().find(|(n, _)| *n == id).map(|(_, f)| f),
        }),
        None => Value::Null,
    }
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
    let sources: Vec<Uuid> = doc.derived_from.into_iter().collect();
    let names = store::document_filenames(&mut cx.tx, &sources).await?;
    let mut split_into = Vec::new();
    for part in store::derived_documents(&mut cx.tx, doc.id).await? {
        let pages: Vec<u32> = current_arrangement(cx, part.id)
            .await?
            .map(|a| a.pages.iter().map(|p| p.page).collect())
            .unwrap_or_default();
        split_into.push((pages, part));
    }
    // In page order; a part given back (no pages) last.
    split_into.sort_by_key(|(pages, _)| pages.first().copied().unwrap_or(u32::MAX));
    let split_into: Vec<Value> = split_into
        .into_iter()
        .map(|(pages, part)| {
            json!({ "id": part.id, "filename": part.filename, "status": part.status,
                    "bill_id": part.bill_id, "pages": pages })
        })
        .collect();
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
               "pages": pages, "extractions": extractions, "history": history,
               "split_from": split_from(&doc, &names), "split_into": split_into }),
    )
}

/// The pages as an arrangement shows them; the original's when there is
/// none. None when the page count isn't known.
/// A document split from another always has an arrangement (its first
/// version is its pages), so the fallback applies to originals only.
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
    if doc.status == "split" {
        return Ok(Some(
            "This document has no pages left: they were moved to other documents".into(),
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
    let old = arranged(current, Some(count)).unwrap_or_default();
    pages::validate_within(&input.pages, &old, count)
        .map_err(|e| CommandError::Invalid(e.to_string()))?;
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

/// The document a new bill is filed with: `document_id` itself, or, when
/// `pages` leaves out some of its visible pages, a new document of just
/// those, split from it. Pages naming every visible page file the whole
/// document. Returns the document and, after a split, where it came from.
pub(super) async fn for_bill(
    cx: &mut Cx<'_>,
    document_id: Uuid,
    pages: Option<&[u32]>,
    expected_version: Option<i32>,
) -> Result<(Uuid, Option<Uuid>), CommandError> {
    let doc = store::get_document(&mut cx.tx, document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    if let Some(expected) = expected_version {
        let version = current_arrangement(cx, doc.id)
            .await?
            .map_or(0, |a| a.version);
        if version != expected {
            return Err(CommandError::Conflict(format!(
                "the pages changed since version {expected}: they are at version {version}. \
                 Get the document again."
            )));
        }
    }
    let Some(pages) = pages else {
        return Ok((doc.id, None));
    };
    if doc.status != "inbox" {
        // file_with_bill says why, as it does for a whole document.
        return Ok((doc.id, None));
    }
    let count = page_count(cx, &doc).await?;
    let current = current_arrangement(cx, doc.id).await?;
    let visible: Vec<u32> = arranged(current.as_ref(), count)
        .unwrap_or_default()
        .iter()
        .filter(|p| !p.hidden)
        .map(|p| p.page)
        .collect();
    if !visible.is_empty()
        && visible.iter().all(|v| pages.contains(v))
        && pages.len() == visible.len()
    {
        return Ok((doc.id, None));
    }
    let parts = split_pages(cx, &doc, expected_version, &[pages.to_vec()]).await?;
    let part = parts
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("a split of one group made no document"))?;
    Ok((part.id, Some(doc.id)))
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

// ── Splitting ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitInput {
    document_id: Uuid,
    expected_version: i32,
    groups: Vec<Vec<u32>>,
}

/// Moves groups of pages into new documents of their own, each a view of
/// the same original file. The rest stays with this document.
pub async fn split(cx: &mut Cx<'_>, input: SplitInput) -> Result<Value, CommandError> {
    let doc = store::get_document(&mut cx.tx, input.document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    let parts = split_pages(cx, &doc, Some(input.expected_version), &input.groups).await?;
    let doc = store::get_document(&mut cx.tx, doc.id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    let mut out = detail(cx, doc).await?;
    out["documents"] = json!(parts);
    Ok(out)
}

/// The split itself, shared by `document.split`, `bill.create` with pages
/// and the reader's split on the invoices it found. `doc` is locked by the
/// caller. Returns the new documents, in the order of `groups`.
pub(super) async fn split_pages(
    cx: &mut Cx<'_>,
    doc: &store::Document,
    expected_version: Option<i32>,
    groups: &[Vec<u32>],
) -> Result<Vec<store::Document>, CommandError> {
    if let Some(reason) = lock(cx, doc).await? {
        return Err(CommandError::Conflict(reason));
    }
    let count = page_count(cx, doc).await?.ok_or_else(|| {
        CommandError::Conflict(
            "this file can't be read as a PDF, so its pages can't be split".into(),
        )
    })?;
    let versions = store::document_arrangements(&mut cx.tx, doc.id).await?;
    let current = versions.first();
    let version = current.map_or(0, |a| a.version);
    if let Some(expected) = expected_version
        && expected != version
    {
        let by = current.map_or("", |a| a.created_by.as_str());
        return Err(CommandError::Conflict(format!(
            "the pages changed since version {expected}: {by} made version {version}. \
             Get the document again and split that."
        )));
    }
    let old = arranged(current, Some(count)).unwrap_or_default();
    let split = pages::split(&old, groups).map_err(|e| CommandError::Invalid(e.to_string()))?;
    let left_visible = split.remaining.iter().any(|p| !p.hidden);
    if doc.bill_id.is_some() && !left_visible {
        return Err(CommandError::Conflict(
            "this document is filed with a bill, so it must keep a page to show".into(),
        ));
    }

    let moved: Vec<Vec<u32>> = split
        .parts
        .iter()
        .map(|part| part.iter().map(|p| p.page).collect())
        .collect();
    let summary = match moved.as_slice() {
        [one] => format!("split {} into a new document", pages::describe_pages(one)),
        many => {
            let each: Vec<String> = many.iter().map(|m| pages::describe_pages(m)).collect();
            format!(
                "split {} into {} new documents",
                join_words(&each),
                many.len()
            )
        }
    };
    let row = store::insert_arrangement(
        &mut cx.tx,
        cx.org,
        doc.id,
        version + 1,
        &split.remaining,
        cx.now,
        &cx.actor.name,
    )
    .await?;

    let mut created = Vec::with_capacity(split.parts.len());
    for (part, numbers) in split.parts.iter().zip(&moved) {
        let filename = pages::part_filename(&doc.filename, numbers);
        let new = store::insert_derived_document(
            &mut cx.tx,
            cx.org,
            doc,
            &filename,
            &cx.actor.name,
            Some(count as i32),
        )
        .await?;
        let first =
            store::insert_arrangement(&mut cx.tx, cx.org, new.id, 1, part, cx.now, &cx.actor.name)
                .await?;
        cx.emit(
            "document.split_from",
            new.id,
            json!({ "document_id": new.id, "source_id": doc.id, "source_filename": doc.filename,
                    "pages": numbers, "arrangement_id": first.id,
                    "summary": format!("split this from {} ({})", doc.filename, pages::describe_pages(numbers)) }),
        )
        .await?;
        created.push(new);
    }
    let parts: Vec<Value> = created
        .iter()
        .zip(&moved)
        .map(|(d, m)| json!({ "document_id": d.id, "filename": d.filename, "pages": m }))
        .collect();
    cx.emit(
        "document.split",
        doc.id,
        json!({ "document_id": doc.id, "arrangement_id": row.id, "version": row.version,
                "previous_version": version, "parts": parts, "summary": summary }),
    )
    .await?;
    if !left_visible {
        store::set_split(&mut cx.tx, doc.id, true).await?;
    }
    // Not locked, so a bill this is filed with is open and unapproved: it
    // moves to the pages that stay, on its own record.
    if let Some(bill_id) = doc.bill_id {
        store::pin_arrangement(&mut cx.tx, bill_id, row.id).await?;
        cx.emit(
            "bill.pages_arranged",
            bill_id,
            json!({ "bill_id": bill_id, "document_id": doc.id, "arrangement_id": row.id,
                    "version": row.version, "summary": summary }),
        )
        .await?;
    }
    Ok(created)
}

/// "a, b and c"
fn join_words(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unsplit {
    document_id: Uuid,
}

/// Undoes a split: a document's pages go back to the document they were
/// split from, which comes back to To review if it had none left. Refused
/// once the split document is a bill or set aside, or when the pages it
/// would go back to are locked.
pub async fn unsplit(cx: &mut Cx<'_>, input: Unsplit) -> Result<Value, CommandError> {
    let doc = store::get_document(&mut cx.tx, input.document_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    let Some(source_id) = doc.derived_from else {
        return Err(CommandError::Invalid(
            "this document wasn't split from another, so there is nothing to undo".into(),
        ));
    };
    match (doc.status.as_str(), doc.bill_id) {
        (_, Some(bill)) => {
            return Err(CommandError::Conflict(format!(
                "this document is filed with bill {bill}, so its pages stay with it"
            )));
        }
        ("trashed", _) => {
            return Err(CommandError::Conflict(
                "this document was set aside as not a bill, so its pages stay with it".into(),
            ));
        }
        ("split", _) => {
            return Err(CommandError::Conflict(
                "this document has no pages left to give back".into(),
            ));
        }
        _ => {}
    }
    let source = store::get_document(&mut cx.tx, source_id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    if source.status != "split"
        && let Some(reason) = lock(cx, &source).await?
    {
        return Err(CommandError::Conflict(format!(
            "{} can't take its pages back. {reason}",
            source.filename
        )));
    }
    let count = page_count(cx, &source).await?.ok_or_else(|| {
        anyhow::anyhow!(
            "document {} was split but its pages can't be counted",
            source.id
        )
    })?;
    let mine = current_arrangement(cx, doc.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("split document {} has no pages", doc.id))?;
    let theirs = current_arrangement(cx, source.id).await?;
    let source_version = theirs.as_ref().map_or(0, |a| a.version);
    let old = arranged(theirs.as_ref(), Some(count)).unwrap_or_default();
    let back = pages::rejoin(&old, &mine.pages);
    let numbers: Vec<u32> = mine.pages.iter().map(|p| p.page).collect();
    let described = pages::describe_pages(&numbers);

    let row = store::insert_arrangement(
        &mut cx.tx,
        cx.org,
        source.id,
        source_version + 1,
        &back,
        cx.now,
        &cx.actor.name,
    )
    .await?;
    store::insert_arrangement(
        &mut cx.tx,
        cx.org,
        doc.id,
        mine.version + 1,
        &[],
        cx.now,
        &cx.actor.name,
    )
    .await?;
    store::set_split(&mut cx.tx, doc.id, true).await?;
    if source.status == "split" {
        store::set_split(&mut cx.tx, source.id, false).await?;
    }
    let summary = format!("took back {described} from {}", doc.filename);
    cx.emit(
        "document.unsplit",
        source.id,
        json!({ "document_id": source.id, "from_document_id": doc.id, "pages": numbers,
                "arrangement_id": row.id, "version": row.version, "previous_version": source_version,
                "summary": summary }),
    )
    .await?;
    cx.emit(
        "document.unsplit",
        doc.id,
        json!({ "document_id": doc.id, "to_document_id": source.id, "pages": numbers,
                "summary": format!("gave {described} back to {}", source.filename) }),
    )
    .await?;
    if let Some(bill_id) = source.bill_id {
        store::pin_arrangement(&mut cx.tx, bill_id, row.id).await?;
        cx.emit(
            "bill.pages_arranged",
            bill_id,
            json!({ "bill_id": bill_id, "document_id": source.id, "arrangement_id": row.id,
                    "version": row.version, "summary": summary }),
        )
        .await?;
    }
    let source = store::get_document(&mut cx.tx, source.id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    detail(cx, source).await
}

/// The visible pages of a document in its order, when the model should
/// read only those: always for a split document, and for an original whose
/// pages were hidden or reordered. None to read the whole file as it is.
pub(super) async fn pages_to_read(
    cx: &mut Cx<'_>,
    doc: &store::Document,
) -> Result<(Vec<u32>, Option<Vec<u32>>), CommandError> {
    let count = page_count(cx, doc).await?;
    let current = current_arrangement(cx, doc.id).await?;
    let all: Vec<u32> = arranged(current.as_ref(), count)
        .unwrap_or_default()
        .iter()
        .map(|p| p.page)
        .collect();
    let visible: Vec<u32> = arranged(current.as_ref(), count)
        .unwrap_or_default()
        .iter()
        .filter(|p| !p.hidden)
        .map(|p| p.page)
        .collect();
    let whole: Vec<u32> = count.map_or_else(Vec::new, |n| (1..=n).collect());
    let hint = (count.is_some() && visible != whole).then_some(visible);
    Ok((all, hint))
}
