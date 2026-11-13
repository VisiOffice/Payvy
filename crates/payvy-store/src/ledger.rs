//! Ledger connection, reference cache, and the per-item post log.
//! Tokens stay sealed bytes. Callers do not put them in events.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::Tx;

#[derive(Clone)]
pub struct LedgerConnection {
    pub id: Uuid,
    /// Where the ledger lives (a VisiBooks base URL). Null for providers
    /// that don't take one; a VisiBooks connection can't post without it.
    pub origin: Option<String>,
    pub provider: String,
    pub environment: String,
    pub account_id: String,
    pub subsidiary_id: Option<String>,
    pub subsidiary_name: Option<String>,
    pub currency: String,
    pub status: String,
    pub auth_kind: String,
    pub token_sealed: Vec<u8>,
    pub post_when: String,
    pub ap_account_id: Option<String>,
    pub defaults: Value,
    pub bank_account_map: Value,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

impl std::fmt::Debug for LedgerConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LedgerConnection")
            .field("id", &self.id)
            .field("origin", &self.origin)
            .field("provider", &self.provider)
            .field("environment", &self.environment)
            .field("account_id", &self.account_id)
            .field("subsidiary_id", &self.subsidiary_id)
            .field("subsidiary_name", &self.subsidiary_name)
            .field("currency", &self.currency)
            .field("status", &self.status)
            .field("auth_kind", &self.auth_kind)
            .field("token_sealed", &"[sealed]")
            .field("post_when", &self.post_when)
            .field("ap_account_id", &self.ap_account_id)
            .field("defaults", &self.defaults)
            .field("bank_account_map", &self.bank_account_map)
            .field("created_at", &self.created_at)
            .field("created_by", &self.created_by)
            .finish()
    }
}

pub struct NewLedgerConnection<'a> {
    pub id: Uuid,
    pub origin: Option<&'a str>,
    pub provider: &'a str,
    pub environment: &'a str,
    pub account_id: &'a str,
    pub subsidiary_id: Option<&'a str>,
    pub subsidiary_name: Option<&'a str>,
    pub currency: &'a str,
    pub status: &'a str,
    pub auth_kind: &'a str,
    pub token_sealed: &'a [u8],
    pub post_when: &'a str,
    pub ap_account_id: Option<&'a str>,
    pub defaults: &'a Value,
    pub bank_account_map: &'a Value,
    pub created_at: DateTime<Utc>,
    pub created_by: &'a str,
}

fn connection(row: &sqlx::postgres::PgRow) -> LedgerConnection {
    LedgerConnection {
        id: row.get("id"),
        origin: row.get("origin"),
        provider: row.get("provider"),
        environment: row.get("environment"),
        account_id: row.get("account_id"),
        subsidiary_id: row.get("subsidiary_id"),
        subsidiary_name: row.get("subsidiary_name"),
        currency: row.get("currency"),
        status: row.get("status"),
        auth_kind: row.get("auth_kind"),
        token_sealed: row.get("token_sealed"),
        post_when: row.get("post_when"),
        ap_account_id: row.get("ap_account_id"),
        defaults: row.get("defaults"),
        bank_account_map: row.get("bank_account_map"),
        created_at: row.get("created_at"),
        created_by: row.get("created_by"),
    }
}

macro_rules! connection_cols {
    () => {
"id, origin, provider, environment, account_id, subsidiary_id, subsidiary_name, currency, status, auth_kind, token_sealed, post_when, ap_account_id, defaults, bank_account_map, created_at, created_by"
    };
}

pub async fn insert_ledger_connection(
    tx: &mut Tx,
    org_id: Uuid,
    new: &NewLedgerConnection<'_>,
) -> anyhow::Result<LedgerConnection> {
    let row = sqlx::query(concat!(
        "INSERT INTO ledger_connections (id, org_id, origin, provider, environment, account_id, subsidiary_id,
            subsidiary_name, currency, status, auth_kind, token_sealed, post_when, ap_account_id,
            defaults, bank_account_map, created_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
         RETURNING ",
        connection_cols!()
    ))
        .bind(new.id)
        .bind(org_id)
        .bind(new.origin)
        .bind(new.provider)
        .bind(new.environment)
        .bind(new.account_id)
        .bind(new.subsidiary_id)
        .bind(new.subsidiary_name)
        .bind(new.currency)
        .bind(new.status)
        .bind(new.auth_kind)
        .bind(new.token_sealed)
        .bind(new.post_when)
        .bind(new.ap_account_id)
        .bind(new.defaults)
        .bind(new.bank_account_map)
        .bind(new.created_at)
        .bind(new.created_by)
        .fetch_one(&mut **tx)
        .await?;
    Ok(connection(&row))
}

/// The company's one active connection, locked for the rest of the transaction.
///
/// The lock is held across the ledger call. That serializes every ledger
/// action in the company, and it is what stops two first posts of the same
/// bill from both reaching the books. Acceptable until a real adapter exists.
pub async fn active_ledger_connection(tx: &mut Tx) -> anyhow::Result<Option<LedgerConnection>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        connection_cols!(),
        " FROM ledger_connections WHERE status = 'active' FOR UPDATE"
    ))
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(connection))
}

/// An Admin's settings change on the active connection: where it lives,
/// what it posts as, when it posts, and how banks and defaults map. A new
/// sealed token replaces the old when one is given; the row (and so its
/// links) is the same connection.
#[allow(clippy::too_many_arguments)]
pub async fn update_ledger_connection(
    tx: &mut Tx,
    id: Uuid,
    origin: Option<&str>,
    account_id: &str,
    post_when: &str,
    ap_account_id: Option<&str>,
    defaults: &Value,
    bank_account_map: &Value,
    token_sealed: Option<&[u8]>,
) -> anyhow::Result<()> {
    // The entity is also the books' subsidiary: posting refuses without one.
    sqlx::query(
        "UPDATE ledger_connections SET origin = $2, account_id = $3, subsidiary_id = $3, post_when = $4, ap_account_id = $5, defaults = $6, bank_account_map = $7, token_sealed = COALESCE($8, token_sealed) WHERE id = $1",
    )
    .bind(id)
    .bind(origin)
    .bind(account_id)
    .bind(post_when)
    .bind(ap_account_id)
    .bind(defaults)
    .bind(bank_account_map)
    .bind(token_sealed)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Retire a connection so a different one can be the active one. Its
/// links stay with it; a new connection starts with none.
pub async fn disable_ledger_connection(tx: &mut Tx, id: Uuid) -> anyhow::Result<()> {
    sqlx::query("UPDATE ledger_connections SET status = 'disabled' WHERE id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn set_ledger_bank_map(tx: &mut Tx, id: Uuid, map: &Value) -> anyhow::Result<()> {
    sqlx::query("UPDATE ledger_connections SET bank_account_map = $2 WHERE id = $1")
        .bind(id)
        .bind(map)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LedgerRecord {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub kind: String,
    pub external_id: String,
    pub name: String,
    pub parent_external_id: Option<String>,
    pub inactive: bool,
    pub detail: Value,
}

pub struct NewLedgerRecord<'a> {
    pub connection_id: Uuid,
    pub kind: &'a str,
    pub external_id: &'a str,
    pub name: &'a str,
    pub parent_external_id: Option<&'a str>,
    pub inactive: bool,
    pub detail: &'a Value,
}

pub async fn insert_ledger_record(
    tx: &mut Tx,
    org_id: Uuid,
    new: &NewLedgerRecord<'_>,
) -> anyhow::Result<LedgerRecord> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        "INSERT INTO ledger_records (id, org_id, connection_id, kind, external_id, name, parent_external_id, inactive, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING id, connection_id, kind, external_id, name, parent_external_id, inactive, detail",
    )
    .bind(id)
    .bind(org_id)
    .bind(new.connection_id)
    .bind(new.kind)
    .bind(new.external_id)
    .bind(new.name)
    .bind(new.parent_external_id)
    .bind(new.inactive)
    .bind(new.detail)
    .fetch_one(&mut **tx)
    .await?;
    Ok(record(&row))
}

pub async fn ledger_records(tx: &mut Tx, connection_id: Uuid) -> anyhow::Result<Vec<LedgerRecord>> {
    let rows = sqlx::query(
        "SELECT id, connection_id, kind, external_id, name, parent_external_id, inactive, detail
         FROM ledger_records WHERE connection_id = $1 ORDER BY kind, external_id",
    )
    .bind(connection_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(record).collect())
}

fn record(row: &sqlx::postgres::PgRow) -> LedgerRecord {
    LedgerRecord {
        id: row.get("id"),
        connection_id: row.get("connection_id"),
        kind: row.get("kind"),
        external_id: row.get("external_id"),
        name: row.get("name"),
        parent_external_id: row.get("parent_external_id"),
        inactive: row.get("inactive"),
        detail: row.get("detail"),
    }
}

#[derive(Debug, Clone)]
pub struct LedgerLink {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub entity: String,
    pub payvy_id: Uuid,
    pub external_id: Option<String>,
    pub external_key: String,
    pub status: String,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub request_hash: Option<String>,
    pub posted_at: Option<DateTime<Utc>>,
    /// The bank reference last sent to the ledger for this payment.
    pub bank_reference: Option<String>,
}

pub struct PutLedgerLink<'a> {
    pub connection_id: Uuid,
    pub entity: &'a str,
    pub payvy_id: Uuid,
    pub external_id: Option<&'a str>,
    pub external_key: &'a str,
    pub status: &'a str,
    pub error_code: Option<&'a str>,
    pub error_detail: Option<&'a str>,
    pub request_hash: Option<&'a str>,
    pub posted_at: Option<DateTime<Utc>>,
}

macro_rules! link_cols {
    () => {
        "id, connection_id, entity, payvy_id, external_id, external_key, status, error_code, error_detail, request_hash, posted_at, bank_reference"
    };
}

fn link(row: &sqlx::postgres::PgRow) -> LedgerLink {
    LedgerLink {
        id: row.get("id"),
        connection_id: row.get("connection_id"),
        entity: row.get("entity"),
        payvy_id: row.get("payvy_id"),
        external_id: row.get("external_id"),
        external_key: row.get("external_key"),
        status: row.get("status"),
        error_code: row.get("error_code"),
        error_detail: row.get("error_detail"),
        request_hash: row.get("request_hash"),
        posted_at: row.get("posted_at"),
        bank_reference: row.get("bank_reference"),
    }
}

/// Locks the link when it exists.
pub async fn ledger_link(
    tx: &mut Tx,
    connection_id: Uuid,
    entity: &str,
    payvy_id: Uuid,
) -> anyhow::Result<Option<LedgerLink>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        link_cols!(),
        " FROM ledger_links WHERE connection_id = $1 AND entity = $2 AND payvy_id = $3 FOR UPDATE"
    ))
    .bind(connection_id)
    .bind(entity)
    .bind(payvy_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(link))
}

/// Inserts or replaces the post result. A null external id, hash, or posted_at
/// keeps the value already stored, so an error write does not wipe a ledger id.
pub async fn put_ledger_link(
    tx: &mut Tx,
    org_id: Uuid,
    new: &PutLedgerLink<'_>,
) -> anyhow::Result<LedgerLink> {
    let row = sqlx::query(concat!(
        "INSERT INTO ledger_links (id, org_id, connection_id, entity, payvy_id, external_id, external_key,
            status, error_code, error_detail, request_hash, posted_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         ON CONFLICT (org_id, connection_id, entity, payvy_id) DO UPDATE SET
            external_id = coalesce(EXCLUDED.external_id, ledger_links.external_id),
            external_key = EXCLUDED.external_key,
            status = EXCLUDED.status,
            error_code = EXCLUDED.error_code,
            error_detail = EXCLUDED.error_detail,
            request_hash = coalesce(EXCLUDED.request_hash, ledger_links.request_hash),
            posted_at = coalesce(EXCLUDED.posted_at, ledger_links.posted_at)
         RETURNING ",
        link_cols!()
    ))
        .bind(Uuid::new_v4())
        .bind(org_id)
        .bind(new.connection_id)
        .bind(new.entity)
        .bind(new.payvy_id)
        .bind(new.external_id)
        .bind(new.external_key)
        .bind(new.status)
        .bind(new.error_code)
        .bind(new.error_detail)
        .bind(new.request_hash)
        .bind(new.posted_at)
        .fetch_one(&mut **tx)
        .await?;
    Ok(link(&row))
}

/// Records a refusal on a link that stays in its current status.
pub async fn note_ledger_link_error(
    tx: &mut Tx,
    connection_id: Uuid,
    entity: &str,
    payvy_id: Uuid,
    error_code: &str,
    error_detail: &str,
) -> anyhow::Result<()> {
    let done = sqlx::query(
        "UPDATE ledger_links SET error_code = $4, error_detail = $5
         WHERE connection_id = $1 AND entity = $2 AND payvy_id = $3",
    )
    .bind(connection_id)
    .bind(entity)
    .bind(payvy_id)
    .bind(error_code)
    .bind(error_detail)
    .execute(&mut **tx)
    .await?;
    anyhow::ensure!(
        done.rows_affected() == 1,
        "ledger link to annotate was not there"
    );
    Ok(())
}

pub async fn mark_ledger_link_voided(
    tx: &mut Tx,
    connection_id: Uuid,
    entity: &str,
    payvy_id: Uuid,
) -> anyhow::Result<()> {
    let done = sqlx::query(
        "UPDATE ledger_links SET status = 'voided', error_code = NULL, error_detail = NULL
         WHERE connection_id = $1 AND entity = $2 AND payvy_id = $3",
    )
    .bind(connection_id)
    .bind(entity)
    .bind(payvy_id)
    .execute(&mut **tx)
    .await?;
    anyhow::ensure!(
        done.rows_affected() == 1,
        "ledger link to void was not there"
    );
    Ok(())
}

// ── Queued posting jobs ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LedgerJob {
    pub id: Uuid,
    pub kind: String,
    pub bill_id: Option<Uuid>,
    pub payment_id: Option<Uuid>,
    pub payload: Value,
    pub attempts: i32,
}

/// Queue a job unless an identical live one already stands. Inserted in
/// the caller's transaction: the job exists exactly when the fact that
/// caused it committed.
pub async fn enqueue_ledger_job(
    tx: &mut Tx,
    org: Uuid,
    kind: &str,
    bill_id: Option<Uuid>,
    payment_id: Option<Uuid>,
    payload: &Value,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO ledger_jobs (id, org_id, kind, bill_id, payment_id, payload, next_attempt_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7, $7) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4()).bind(org).bind(kind).bind(bill_id).bind(payment_id).bind(payload).bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Claim due jobs for processing: they leave as `sending` in this
/// transaction, so two workers never take the same job.
pub async fn claim_ledger_jobs(
    tx: &mut Tx,
    now: DateTime<Utc>,
    limit: i64,
) -> anyhow::Result<Vec<LedgerJob>> {
    let rows = sqlx::query(
        "SELECT id, kind, bill_id, payment_id, payload, attempts FROM ledger_jobs \
         WHERE status IN ('pending', 'failed') AND next_attempt_at <= $1 \
         ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::new();
    for r in &rows {
        let id: Uuid = r.get("id");
        sqlx::query("UPDATE ledger_jobs SET status = 'sending', attempts = attempts + 1, updated_at = $2 WHERE id = $1")
            .bind(id)
            .bind(now)
            .execute(&mut **tx)
            .await?;
        out.push(LedgerJob {
            id,
            kind: r.get("kind"),
            bill_id: r.get("bill_id"),
            payment_id: r.get("payment_id"),
            payload: r.get("payload"),
            attempts: r.get::<i32, _>("attempts") + 1,
        });
    }
    Ok(out)
}

pub async fn finish_ledger_job(tx: &mut Tx, id: Uuid, now: DateTime<Utc>) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE ledger_jobs SET status = 'done', last_error = NULL, updated_at = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A failed attempt waits with backoff (attempts squared minutes, capped
/// at an hour) and keeps its error where a person can see it.
pub async fn fail_ledger_job(
    tx: &mut Tx,
    id: Uuid,
    error: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let attempts: i32 = sqlx::query("SELECT attempts FROM ledger_jobs WHERE id = $1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?
        .get("attempts");
    let backoff = chrono::Duration::minutes(i64::from(attempts * attempts).min(60));
    sqlx::query(
        "UPDATE ledger_jobs SET status = 'failed', last_error = $2, next_attempt_at = $3, updated_at = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .bind(now + backoff)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn set_ledger_link_bank_reference(
    tx: &mut Tx,
    connection_id: Uuid,
    payment_id: Uuid,
    reference: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE ledger_links SET bank_reference = $3 WHERE connection_id = $1 AND entity = 'payment' AND payvy_id = $2",
    )
    .bind(connection_id)
    .bind(payment_id)
    .bind(reference)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
