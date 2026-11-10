//! Email intake: the inbound address, and reading an email into the inbox.

use base64::Engine;
use payvy_domain::email::{self as rules, Skipped};
use payvy_store::{self as store, NewEmail};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::documents::{self, Received};
use super::{CommandError, Cx, text};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

pub async fn address(cx: &mut Cx<'_>, _: Empty) -> Result<Value, CommandError> {
    let domain = cx.inbound_domain()?;
    Ok(
        match store::live_inbound_address(&mut cx.tx, cx.org).await? {
            Some(a) => json!({
                "address": format!("{}@{domain}", a.token),
                "created_at": a.created_at,
                "created_by": a.created_by,
            }),
            None => {
                json!({ "address": null, "hint": "a person can create one with email.new_address" })
            }
        },
    )
}

pub async fn new_address(cx: &mut Cx<'_>, _: Empty) -> Result<Value, CommandError> {
    let domain = cx.inbound_domain()?.to_owned();
    let old = store::live_inbound_address(&mut cx.tx, cx.org).await?;
    // 31^12 tokens: a collision is vanishingly rare, but cheap to retry.
    for _ in 0..5 {
        let token = new_token()?;
        if store::replace_inbound_address(&mut cx.tx, cx.org, &token, cx.now, &cx.actor.name)
            .await?
        {
            let address = format!("{token}@{domain}");
            let retired = old.as_ref().map(|a| format!("{}@{domain}", a.token));
            cx.emit(
                "email.address_created",
                cx.org,
                json!({ "address": address, "retired": retired }),
            )
            .await?;
            return Ok(json!({ "address": address, "retired": retired }));
        }
    }
    Err(anyhow::anyhow!("couldn't find an unused inbound token").into())
}

fn new_token() -> Result<String, CommandError> {
    let alphabet = rules::TOKEN_ALPHABET;
    // Rejection sampling keeps every character equally likely.
    let limit = (256 / alphabet.len() * alphabet.len()) as u8;
    let mut token = String::with_capacity(rules::TOKEN_LEN);
    while token.len() < rules::TOKEN_LEN {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
        for b in buf {
            if b < limit && token.len() < rules::TOKEN_LEN {
                token.push(alphabet[b as usize % alphabet.len()] as char);
            }
        }
    }
    Ok(token)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    raw_base64: String,
    recipient: Option<String>,
}

pub async fn import(cx: &mut Cx<'_>, input: Import) -> Result<Value, CommandError> {
    let storage = cx.storage()?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(input.raw_base64.trim())
        .map_err(|e| CommandError::Invalid(format!("raw_base64 isn't valid base64: {e}")))?;
    let parsed = rules::parse(&raw).map_err(|e| CommandError::Invalid(e.to_string()))?;
    let sha = hex::encode(Sha256::digest(&raw));
    if let Some(existing) =
        store::existing_email(&mut cx.tx, parsed.message_id.as_deref(), &sha).await?
    {
        return Err(already_here(&existing));
    }

    let key = format!("orgs/{}/emails/{sha}.eml", cx.org);
    let size_bytes = raw.len() as i64;
    if !cx.preview {
        storage.put(&key, raw, "message/rfc822").await?;
    }
    let recipient = text(input.recipient);
    let id = Uuid::new_v4();
    let new = NewEmail {
        id,
        message_id: parsed.message_id.as_deref(),
        raw_sha256: &sha,
        size_bytes,
        storage_key: &key,
        recipient: recipient.as_deref(),
        from_address: parsed.from_address.as_deref(),
        from_name: parsed.from_name.as_deref(),
        subject: parsed.subject.as_deref(),
        sent_at: parsed.sent_at,
        authentication_results: parsed.authentication_results.as_deref(),
        received_at: cx.now,
        created_by: &cx.actor.name,
    };
    if !store::insert_email(&mut cx.tx, cx.org, &new).await? {
        let existing = store::existing_email(&mut cx.tx, parsed.message_id.as_deref(), &sha)
            .await?
            .ok_or_else(|| anyhow::anyhow!("email {sha} conflicted but can't be read"))?;
        return Err(already_here(&existing));
    }

    let mut skipped = parsed.skipped;
    let mut document_ids = Vec::new();
    for a in parsed.attachments {
        match documents::receive(cx, a.bytes, a.kind, &a.filename, Some(id)).await? {
            Received::New(doc) => document_ids.push(doc.id),
            Received::AlreadyHere(existing) => skipped.push(Skipped {
                filename: a.filename,
                reason: format!("already here as {}", documents::where_is(&existing)),
            }),
        }
    }
    if !skipped.is_empty() {
        store::set_email_skipped(&mut cx.tx, id, &json!(skipped)).await?;
    }
    cx.emit(
        "email.received",
        id,
        json!({
            "email_id": id,
            "from": parsed.from_address,
            "subject": parsed.subject,
            "document_ids": document_ids,
            "skipped": skipped.len(),
        }),
    )
    .await?;
    detail(cx, id).await
}

fn already_here(existing: &store::Email) -> CommandError {
    CommandError::Conflict(format!(
        "this email is already here: email {} from {}, received {}",
        existing.id,
        existing
            .from_address
            .as_deref()
            .unwrap_or("an unknown sender"),
        existing.received_at.to_rfc3339()
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    limit: Option<i64>,
}

pub async fn list(cx: &mut Cx<'_>, input: List) -> Result<Value, CommandError> {
    let limit = input.limit.unwrap_or(50).clamp(1, 500);
    Ok(json!({ "emails": store::list_emails(&mut cx.tx, limit).await? }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Get {
    email_id: Uuid,
}

pub async fn get(cx: &mut Cx<'_>, input: Get) -> Result<Value, CommandError> {
    let mut out = detail(cx, input.email_id).await?;
    let key = store::get_email(&mut cx.tx, input.email_id)
        .await?
        .ok_or(CommandError::NotFound("email"))?
        .storage_key;
    out["download_url"] = json!(cx.storage()?.download_url(&key));
    out["download_url_expires_in_seconds"] = json!(900);
    Ok(out)
}

async fn detail(cx: &mut Cx<'_>, id: Uuid) -> Result<Value, CommandError> {
    let email = store::get_email(&mut cx.tx, id)
        .await?
        .ok_or(CommandError::NotFound("email"))?;
    let docs = store::email_documents(&mut cx.tx, id).await?;
    Ok(json!({ "email": email, "documents": docs }))
}
