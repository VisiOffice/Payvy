//! Postgres storage. Every write happens inside an organization-scoped
//! transaction and appends an event in that same transaction, so a change
//! and its event either both land or neither does.

use chrono::{DateTime, NaiveDate, Utc};
use payvy_domain::Cents;
use payvy_domain::bills::{Line, State};
use payvy_domain::workflow::{Approval, Signal, Workflow};
use serde::Serialize;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

pub type Tx = Transaction<'static, Postgres>;

pub const DEFAULT_ORG: Uuid = Uuid::from_u128(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Person,
    Agent,
}

impl ActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActorKind::Person => "person",
            ActorKind::Agent => "agent",
        }
    }
}

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// A transaction scoped to one organization by row-level security.
    pub async fn begin(&self, org_id: Uuid) -> anyhow::Result<Tx> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('payvy.org_id', $1, true)")
            .bind(org_id.to_string())
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// The organization, name and kind for an API key; None if unknown or revoked.
    pub async fn actor_for_key_hash(
        &self,
        key_hash: &str,
    ) -> anyhow::Result<Option<(Uuid, String, ActorKind)>> {
        let row = sqlx::query(
            "SELECT org_id, actor, kind FROM api_keys WHERE key_hash = $1 AND revoked_at IS NULL",
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            let kind = if r.get::<String, _>("kind") == "person" {
                ActorKind::Person
            } else {
                ActorKind::Agent
            };
            (r.get("org_id"), r.get("actor"), kind)
        }))
    }

    pub async fn insert_api_key(
        &self,
        org_id: Uuid,
        actor: &str,
        kind: ActorKind,
        key_hash: &str,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO api_keys (id, org_id, actor, kind, key_hash) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(org_id)
        .bind(actor)
        .bind(kind.as_str())
        .bind(key_hash)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }
}

// ── Events and confirm tokens ───────────────────────────────────────────────

pub async fn append_event(
    tx: &mut Tx,
    org_id: Uuid,
    kind: &str,
    entity_id: Option<Uuid>,
    actor: &str,
    payload: &serde_json::Value,
) -> anyhow::Result<i64> {
    let row = sqlx::query(
        "INSERT INTO events (org_id, kind, entity_id, actor, payload) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(org_id)
    .bind(kind)
    .bind(entity_id)
    .bind(actor)
    .bind(payload)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.get("id"))
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub id: i64,
    pub kind: String,
    pub entity_id: Option<Uuid>,
    pub actor: String,
    pub payload: serde_json::Value,
    pub at: DateTime<Utc>,
}

/// Newest first. `entity` also matches events whose payload names it as
/// `bill_id`, `vendor_id` or `proposal_id`.
pub async fn list_events(
    tx: &mut Tx,
    entity: Option<Uuid>,
    limit: i64,
) -> anyhow::Result<Vec<Event>> {
    let rows = sqlx::query(
        "SELECT id, kind, entity_id, actor, payload, created_at FROM events
         WHERE $1::uuid IS NULL OR entity_id = $1
            OR payload->>'bill_id' = $1::text OR payload->>'vendor_id' = $1::text
            OR payload->>'proposal_id' = $1::text
         ORDER BY id DESC LIMIT $2",
    )
    .bind(entity)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Event {
            id: r.get("id"),
            kind: r.get("kind"),
            entity_id: r.get("entity_id"),
            actor: r.get("actor"),
            payload: r.get("payload"),
            at: r.get("created_at"),
        })
        .collect())
}

/// Marks a confirm token used. False if it was already used, so a retried
/// commit can never apply twice. Runs in the command's own transaction.
pub async fn consume_confirm_token(
    tx: &mut Tx,
    org_id: Uuid,
    token: &str,
    command: &str,
) -> anyhow::Result<bool> {
    let inserted = sqlx::query(
        "INSERT INTO used_confirm_tokens (token, org_id, command) VALUES ($1, $2, $3) ON CONFLICT (token) DO NOTHING",
    )
    .bind(token)
    .bind(org_id)
    .bind(command)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}

// ── Vendors ─────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct Vendor {
    pub id: Uuid,
    pub name: String,
    pub email: Option<String>,
    pub terms_days: Option<i32>,
}

fn vendor(r: &PgRow) -> Vendor {
    Vendor {
        id: r.get("id"),
        name: r.get("name"),
        email: r.get("email"),
        terms_days: r.get("terms_days"),
    }
}

pub async fn insert_vendor(
    tx: &mut Tx,
    org_id: Uuid,
    name: &str,
    email: Option<&str>,
    terms_days: Option<i32>,
) -> anyhow::Result<Vendor> {
    let row = sqlx::query(
        "INSERT INTO vendors (id, org_id, name, email, terms_days) VALUES ($1, $2, $3, $4, $5)
         RETURNING id, name, email, terms_days",
    )
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(name)
    .bind(email)
    .bind(terms_days)
    .fetch_one(&mut **tx)
    .await?;
    Ok(vendor(&row))
}

pub async fn get_vendor(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Vendor>> {
    let row = sqlx::query("SELECT id, name, email, terms_days FROM vendors WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.as_ref().map(vendor))
}

#[derive(Debug, Serialize)]
pub struct VendorSummary {
    #[serde(flatten)]
    pub vendor: Vendor,
    pub open_bills: i64,
    pub open_cents: Cents,
}

pub async fn list_vendors(tx: &mut Tx) -> anyhow::Result<Vec<VendorSummary>> {
    let rows = sqlx::query(
        "SELECT v.id, v.name, v.email, v.terms_days,
                count(b.id) AS open_bills, coalesce(sum(b.amount_cents), 0)::bigint AS open_cents
         FROM vendors v LEFT JOIN bills b ON b.vendor_id = v.id AND b.state = 'open'
         GROUP BY v.id ORDER BY lower(v.name)",
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .iter()
        .map(|r| VendorSummary {
            vendor: vendor(r),
            open_bills: r.get("open_bills"),
            open_cents: r.get("open_cents"),
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct VendorAccount {
    pub id: Uuid,
    pub account_number: String,
    pub location: Option<String>,
}

/// None when the vendor already has this account number.
pub async fn insert_vendor_account(
    tx: &mut Tx,
    org_id: Uuid,
    vendor_id: Uuid,
    account_number: &str,
    location: Option<&str>,
) -> anyhow::Result<Option<VendorAccount>> {
    let row = sqlx::query(
        "INSERT INTO vendor_accounts (id, org_id, vendor_id, account_number, location) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (org_id, vendor_id, account_number) DO NOTHING
         RETURNING id, account_number, location",
    )
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(vendor_id)
    .bind(account_number)
    .bind(location)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| VendorAccount {
        id: r.get("id"),
        account_number: r.get("account_number"),
        location: r.get("location"),
    }))
}

pub async fn vendor_accounts(tx: &mut Tx, vendor_id: Uuid) -> anyhow::Result<Vec<VendorAccount>> {
    let rows = sqlx::query(
        "SELECT id, account_number, location FROM vendor_accounts WHERE vendor_id = $1 ORDER BY account_number",
    )
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| VendorAccount {
            id: r.get("id"),
            account_number: r.get("account_number"),
            location: r.get("location"),
        })
        .collect())
}

// ── Workflows ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct WorkflowRow {
    pub id: Uuid,
    pub vendor_id: Option<Uuid>,
    pub workflow: Workflow,
    pub status: String,
    pub reason: Option<String>,
    pub proposed_by: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

macro_rules! workflow_cols {
    () => {
        "id, vendor_id, definition, status, reason, proposed_by, decided_by, decided_at, created_at"
    };
}

fn workflow_row(r: &PgRow) -> anyhow::Result<WorkflowRow> {
    Ok(WorkflowRow {
        id: r.get("id"),
        vendor_id: r.get("vendor_id"),
        workflow: serde_json::from_value(r.get("definition"))?,
        status: r.get("status"),
        reason: r.get("reason"),
        proposed_by: r.get("proposed_by"),
        decided_by: r.get("decided_by"),
        decided_at: r.get("decided_at"),
        created_at: r.get("created_at"),
    })
}

/// The active workflow for a vendor, or with None the organization default.
/// Doesn't fall back from vendor to default: see `effective_workflow`.
pub async fn active_workflow(
    tx: &mut Tx,
    vendor_id: Option<Uuid>,
) -> anyhow::Result<Option<WorkflowRow>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        workflow_cols!(),
        " FROM workflows WHERE status = 'active' AND vendor_id IS NOT DISTINCT FROM $1"
    ))
    .bind(vendor_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(workflow_row).transpose()
}

/// What a vendor's new bills follow: its own workflow, else the
/// organization default, else None for the built-in default.
pub async fn effective_workflow(
    tx: &mut Tx,
    vendor_id: Uuid,
) -> anyhow::Result<Option<WorkflowRow>> {
    match active_workflow(tx, Some(vendor_id)).await? {
        Some(w) => Ok(Some(w)),
        None => active_workflow(tx, None).await,
    }
}

pub async fn get_workflow(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<WorkflowRow>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        workflow_cols!(),
        " FROM workflows WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(workflow_row).transpose()
}

pub async fn proposals(tx: &mut Tx, vendor_id: Option<Uuid>) -> anyhow::Result<Vec<WorkflowRow>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        workflow_cols!(),
        " FROM workflows WHERE status = 'proposed' AND vendor_id IS NOT DISTINCT FROM $1
         ORDER BY created_at"
    ))
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(workflow_row).collect()
}

pub async fn insert_proposal(
    tx: &mut Tx,
    org_id: Uuid,
    vendor_id: Option<Uuid>,
    workflow: &Workflow,
    reason: Option<&str>,
    proposed_by: &str,
) -> anyhow::Result<WorkflowRow> {
    let row = sqlx::query(concat!(
        "INSERT INTO workflows (id, org_id, vendor_id, definition, status, reason, proposed_by)
         VALUES ($1, $2, $3, $4, 'proposed', $5, $6) RETURNING ",
        workflow_cols!()
    ))
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(vendor_id)
    .bind(serde_json::to_value(workflow)?)
    .bind(reason)
    .bind(proposed_by)
    .fetch_one(&mut **tx)
    .await?;
    workflow_row(&row)
}

/// Moves a proposal to `active` (superseding the one it replaces) or to
/// `declined`. The caller has checked it is still `proposed`.
pub async fn decide_proposal(
    tx: &mut Tx,
    proposal: &WorkflowRow,
    accept: bool,
    actor: &str,
) -> anyhow::Result<()> {
    if accept {
        sqlx::query(
            "UPDATE workflows SET status = 'superseded' WHERE status = 'active' AND vendor_id IS NOT DISTINCT FROM $1",
        )
        .bind(proposal.vendor_id)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "UPDATE workflows SET status = $2, decided_by = $3, decided_at = now() WHERE id = $1",
    )
    .bind(proposal.id)
    .bind(if accept { "active" } else { "declined" })
    .bind(actor)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ── Bills ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Bill {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub vendor_name: String,
    pub workflow_id: Option<Uuid>,
    pub invoice_number: String,
    pub invoice_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub amount_cents: Cents,
    pub memo: Option<String>,
    pub external_ref: Option<String>,
    pub state: State,
    pub received_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
    pub released_by: Option<String>,
    pub paid_on: Option<NaiveDate>,
    pub paid_method: Option<String>,
    pub paid_reference: Option<String>,
    pub closed_reason: Option<String>,
    pub created_by: String,
}

macro_rules! bill_cols {
    () => {
        "b.id, b.vendor_id, v.name AS vendor_name, b.workflow_id, b.invoice_number, b.invoice_date,
    b.due_date, b.amount_cents, b.memo, b.external_ref, b.state, b.received_at, b.released_at, b.released_by,
    b.paid_on, b.paid_method, b.paid_reference, b.closed_reason, b.created_by"
    };
}

fn bill(r: &PgRow) -> anyhow::Result<Bill> {
    let state: String = r.get("state");
    Ok(Bill {
        id: r.get("id"),
        vendor_id: r.get("vendor_id"),
        vendor_name: r.get("vendor_name"),
        workflow_id: r.get("workflow_id"),
        invoice_number: r.get("invoice_number"),
        invoice_date: r.get("invoice_date"),
        due_date: r.get("due_date"),
        amount_cents: r.get("amount_cents"),
        memo: r.get("memo"),
        external_ref: r.get("external_ref"),
        state: State::parse(&state).ok_or_else(|| anyhow::anyhow!("unknown bill state {state}"))?,
        received_at: r.get("received_at"),
        released_at: r.get("released_at"),
        released_by: r.get("released_by"),
        paid_on: r.get("paid_on"),
        paid_method: r.get("paid_method"),
        paid_reference: r.get("paid_reference"),
        closed_reason: r.get("closed_reason"),
        created_by: r.get("created_by"),
    })
}

pub struct NewBill<'a> {
    pub vendor_id: Uuid,
    pub workflow_id: Option<Uuid>,
    pub invoice_number: &'a str,
    pub invoice_key: &'a str,
    pub invoice_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub amount_cents: Cents,
    pub memo: Option<&'a str>,
    pub external_ref: Option<&'a str>,
    pub lines: &'a [Line],
    pub created_by: &'a str,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum InsertBill {
    Inserted(Uuid),
    /// A live bill from this vendor already has this invoice number.
    Duplicate(Uuid),
    /// A live bill already has this external_ref.
    ExternalRefTaken(Uuid),
}

pub async fn insert_bill(
    tx: &mut Tx,
    org_id: Uuid,
    new: &NewBill<'_>,
    allow_duplicate: bool,
) -> anyhow::Result<InsertBill> {
    if let Some(r) = new.external_ref {
        let found = sqlx::query("SELECT id FROM bills WHERE external_ref = $1 AND state <> 'void'")
            .bind(r)
            .fetch_optional(&mut **tx)
            .await?;
        if let Some(row) = found {
            return Ok(InsertBill::ExternalRefTaken(row.get("id")));
        }
    }
    let dup = sqlx::query(
        "SELECT id FROM bills WHERE vendor_id = $1 AND invoice_key = $2 AND state <> 'void'",
    )
    .bind(new.vendor_id)
    .bind(new.invoice_key)
    .fetch_optional(&mut **tx)
    .await?;
    // The unique index still holds when duplicates are allowed: the key
    // gets a suffix so the original stays the one that matches.
    let key = match (dup, allow_duplicate) {
        (Some(row), false) => return Ok(InsertBill::Duplicate(row.get("id"))),
        (Some(_), true) => format!("{}~{}", new.invoice_key, Uuid::new_v4().simple()),
        (None, _) => new.invoice_key.to_owned(),
    };
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO bills (id, org_id, vendor_id, workflow_id, invoice_number, invoice_key, invoice_date, due_date,
                            amount_cents, memo, external_ref, created_by, received_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(id)
    .bind(org_id)
    .bind(new.vendor_id)
    .bind(new.workflow_id)
    .bind(new.invoice_number)
    .bind(key)
    .bind(new.invoice_date)
    .bind(new.due_date)
    .bind(new.amount_cents)
    .bind(new.memo)
    .bind(new.external_ref)
    .bind(new.created_by)
    .bind(new.received_at)
    .execute(&mut **tx)
    .await?;
    for (i, line) in new.lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO bill_lines (id, org_id, bill_id, position, description, amount_cents, account)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::new_v4())
        .bind(org_id)
        .bind(id)
        .bind(i as i32)
        .bind(&line.description)
        .bind(line.amount_cents)
        .bind(&line.account)
        .execute(&mut **tx)
        .await?;
    }
    Ok(InsertBill::Inserted(id))
}

/// Locks the bill for the rest of the transaction, so two people acting on
/// it at once are serialized.
pub async fn get_bill(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Bill>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        bill_cols!(),
        " FROM bills b JOIN vendors v ON v.id = b.vendor_id WHERE b.id = $1 FOR UPDATE OF b"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(bill).transpose()
}

pub async fn bill_id_by_external_ref(
    tx: &mut Tx,
    external_ref: &str,
) -> anyhow::Result<Option<Uuid>> {
    let row = sqlx::query("SELECT id FROM bills WHERE external_ref = $1 AND state <> 'void'")
        .bind(external_ref)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.map(|r| r.get("id")))
}

pub async fn list_bills(
    tx: &mut Tx,
    state: Option<State>,
    vendor_id: Option<Uuid>,
    limit: i64,
) -> anyhow::Result<Vec<Bill>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        bill_cols!(),
        " FROM bills b JOIN vendors v ON v.id = b.vendor_id
         WHERE ($1::text IS NULL OR b.state = $1) AND ($2::uuid IS NULL OR b.vendor_id = $2)
         ORDER BY b.received_at DESC, b.id LIMIT $3"
    ))
    .bind(state.map(State::as_str))
    .bind(vendor_id)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(bill).collect()
}

pub async fn bill_lines(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Vec<Line>> {
    let rows = sqlx::query("SELECT description, amount_cents, account FROM bill_lines WHERE bill_id = $1 ORDER BY position")
        .bind(bill_id)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| Line {
            description: r.get("description"),
            amount_cents: r.get("amount_cents"),
            account: r.get("account"),
        })
        .collect())
}

/// Workflow definitions by id, for evaluating many bills at once.
pub async fn workflows_by_id(tx: &mut Tx, ids: &[Uuid]) -> anyhow::Result<Vec<(Uuid, Workflow)>> {
    let rows = sqlx::query("SELECT id, definition FROM workflows WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(&mut **tx)
        .await?;
    rows.into_iter()
        .map(|r| Ok((r.get("id"), serde_json::from_value(r.get("definition"))?)))
        .collect()
}

/// Approvals and signals for these bills, keyed by bill.
pub async fn facts(
    tx: &mut Tx,
    bill_ids: &[Uuid],
) -> anyhow::Result<(Vec<(Uuid, Approval)>, Vec<(Uuid, Signal)>)> {
    let a = sqlx::query(
        "SELECT bill_id, step_id, actor, at FROM approvals WHERE bill_id = ANY($1) ORDER BY at",
    )
    .bind(bill_ids)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        (
            r.get("bill_id"),
            Approval {
                step_id: r.get("step_id"),
                actor: r.get("actor"),
                at: r.get("at"),
            },
        )
    })
    .collect();
    let s = sqlx::query("SELECT bill_id, key, at FROM signals WHERE bill_id = ANY($1) ORDER BY at")
        .bind(bill_ids)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|r| {
            (
                r.get("bill_id"),
                Signal {
                    key: r.get("key"),
                    at: r.get("at"),
                },
            )
        })
        .collect();
    Ok((a, s))
}

/// False if this person already approved this step.
pub async fn insert_approval(
    tx: &mut Tx,
    org_id: Uuid,
    bill_id: Uuid,
    step_id: &str,
    actor: &str,
    note: Option<&str>,
    at: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let n = sqlx::query(
        "INSERT INTO approvals (id, org_id, bill_id, step_id, actor, note, at) VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (bill_id, step_id, actor) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(bill_id)
    .bind(step_id)
    .bind(actor)
    .bind(note)
    .bind(at)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(n == 1)
}

pub async fn insert_signal(
    tx: &mut Tx,
    org_id: Uuid,
    bill_id: Uuid,
    key: &str,
    actor: &str,
    note: Option<&str>,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO signals (id, org_id, bill_id, key, actor, note, at) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(Uuid::new_v4())
        .bind(org_id)
        .bind(bill_id)
        .bind(key)
        .bind(actor)
        .bind(note)
        .bind(at)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn release_bill(
    tx: &mut Tx,
    bill_id: Uuid,
    actor: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE bills SET released_at = $2, released_by = $3 WHERE id = $1")
        .bind(bill_id)
        .bind(at)
        .bind(actor)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Closes an open bill as rejected or void, with the reason.
pub async fn close_bill(
    tx: &mut Tx,
    bill_id: Uuid,
    state: State,
    reason: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE bills SET state = $2, closed_reason = $3 WHERE id = $1")
        .bind(bill_id)
        .bind(state.as_str())
        .bind(reason)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn mark_paid(
    tx: &mut Tx,
    bill_id: Uuid,
    paid_on: NaiveDate,
    method: &str,
    reference: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE bills SET state = 'paid', paid_on = $2, paid_method = $3, paid_reference = $4 WHERE id = $1")
        .bind(bill_id)
        .bind(paid_on)
        .bind(method)
        .bind(reference)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
