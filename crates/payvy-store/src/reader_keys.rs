//! A company's own model key for Payvy's reader (`reader_keys`). The key is
//! sealed by the server before it gets here and is never read back in the
//! clear by anything but the reader.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::Tx;

#[derive(Debug, Clone, Serialize)]
pub struct ReaderKey {
    pub provider: String,
    pub model: String,
    #[serde(skip)]
    pub key_sealed: Vec<u8>,
    pub key_last4: String,
    pub set_at: DateTime<Utc>,
    pub set_by: String,
}

pub async fn reader_key(tx: &mut Tx, org: Uuid) -> anyhow::Result<Option<ReaderKey>> {
    let row = sqlx::query(
        "SELECT provider, model, key_sealed, key_last4, set_at, set_by \
         FROM reader_keys WHERE org_id = $1",
    )
    .bind(org)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| ReaderKey {
        provider: r.get("provider"),
        model: r.get("model"),
        key_sealed: r.get("key_sealed"),
        key_last4: r.get("key_last4"),
        set_at: r.get("set_at"),
        set_by: r.get("set_by"),
    }))
}

/// Sets (or replaces) the company's key.
pub async fn put_reader_key(tx: &mut Tx, org: Uuid, key: &ReaderKey) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO reader_keys (org_id, provider, model, key_sealed, key_last4, set_at, set_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (org_id) DO UPDATE SET provider = EXCLUDED.provider, model = EXCLUDED.model, \
         key_sealed = EXCLUDED.key_sealed, key_last4 = EXCLUDED.key_last4, \
         set_at = EXCLUDED.set_at, set_by = EXCLUDED.set_by",
    )
    .bind(org)
    .bind(&key.provider)
    .bind(&key.model)
    .bind(&key.key_sealed)
    .bind(&key.key_last4)
    .bind(key.set_at)
    .bind(&key.set_by)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Removes the company's key. False when there was none.
pub async fn delete_reader_key(tx: &mut Tx, org: Uuid) -> anyhow::Result<bool> {
    let done = sqlx::query("DELETE FROM reader_keys WHERE org_id = $1")
        .bind(org)
        .execute(&mut **tx)
        .await?;
    Ok(done.rows_affected() > 0)
}
