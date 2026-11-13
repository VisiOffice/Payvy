//! Postgres storage. Every write happens inside an organization-scoped
//! transaction and appends an event in that same transaction, so a change
//! and its event either both land or neither does.

pub use payvy_domain::permissions::Role;
mod ai_usage;
pub use ai_usage::*;
mod entitlements;
pub use entitlements::*;
mod invoice_reviews;
pub use invoice_reviews::*;
mod ledger;
pub use ledger::*;
mod notifications;
mod operator;
mod roles;
pub use notifications::*;
pub use operator::*;
pub use roles::*;

use chrono::{DateTime, NaiveDate, Utc};
use payvy_domain::Cents;
use payvy_domain::bills::{Line, State};
use payvy_domain::pages::PageRef;
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

    /// The organization, key id, name and kind for an API key. None if unknown or revoked.
    pub async fn actor_for_key_hash(
        &self,
        key_hash: &str,
    ) -> anyhow::Result<Option<(Uuid, Uuid, String, ActorKind)>> {
        let row = sqlx::query(
            "SELECT id, org_id, actor, kind FROM api_keys WHERE key_hash = $1 AND revoked_at IS NULL",
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
            (r.get("org_id"), r.get("id"), r.get("actor"), kind)
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

/// True when this organization has this unrevoked agent key.
/// `api_keys` has no row-level security, so the organization is an explicit predicate.
pub async fn live_agent_key(tx: &mut Tx, org: Uuid, key_id: Uuid) -> anyhow::Result<bool> {
    let row = sqlx::query(
        "SELECT 1 AS ok FROM api_keys \
         WHERE id = $1 AND org_id = $2 AND kind = 'agent' AND revoked_at IS NULL",
    )
    .bind(key_id)
    .bind(org)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.is_some())
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

/// Replaces a vendor's name, email and payment terms. None when there is no
/// such vendor.
pub async fn update_vendor(
    tx: &mut Tx,
    id: Uuid,
    name: &str,
    email: Option<&str>,
    terms_days: Option<i32>,
) -> anyhow::Result<Option<Vendor>> {
    let row = sqlx::query(
        "UPDATE vendors SET name = $2, email = $3, terms_days = $4 WHERE id = $1
         RETURNING id, name, email, terms_days",
    )
    .bind(id)
    .bind(name)
    .bind(email)
    .bind(terms_days)
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
    /// True when the vendor has its own active workflow rather than the
    /// organization default.
    pub own_workflow: bool,
    /// Workflow proposals for this vendor waiting for a person.
    pub workflow_proposals: i64,
}

pub async fn list_vendors(tx: &mut Tx) -> anyhow::Result<Vec<VendorSummary>> {
    let rows = sqlx::query(
        "SELECT v.id, v.name, v.email, v.terms_days,
                count(b.id) AS open_bills, coalesce(sum(b.amount_cents), 0)::bigint AS open_cents,
                EXISTS (SELECT 1 FROM workflows w WHERE w.vendor_id = v.id AND w.status = 'active')
                    AS own_workflow,
                (SELECT count(*) FROM workflows w WHERE w.vendor_id = v.id AND w.status = 'proposed')
                    AS workflow_proposals
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
            own_workflow: r.get("own_workflow"),
            workflow_proposals: r.get("workflow_proposals"),
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

/// Removes one of a vendor's account numbers. None when the vendor has no
/// such account.
pub async fn delete_vendor_account(
    tx: &mut Tx,
    vendor_id: Uuid,
    account_id: Uuid,
) -> anyhow::Result<Option<VendorAccount>> {
    let row = sqlx::query(
        "DELETE FROM vendor_accounts WHERE id = $1 AND vendor_id = $2
         RETURNING id, account_number, location",
    )
    .bind(account_id)
    .bind(vendor_id)
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
    /// A payment didn't go or came back: an automatic pay step now waits
    /// for a person to release the bill.
    pub release_by_person: bool,
    pub paid_on: Option<NaiveDate>,
    pub paid_method: Option<String>,
    pub paid_reference: Option<String>,
    pub closed_reason: Option<String>,
    pub created_by: String,
    /// The page arrangement of its document it uses; None for the original.
    pub arrangement_id: Option<Uuid>,
    /// The vendor PO policy version in force when this bill was created;
    /// None reads the registry defaults (every bill before 0025).
    pub policy_version_id: Option<Uuid>,
}

macro_rules! bill_cols {
    () => {
        "b.id, b.vendor_id, v.name AS vendor_name, b.workflow_id, b.invoice_number, b.invoice_date,
    b.due_date, b.amount_cents, b.memo, b.external_ref, b.state, b.received_at, b.released_at, b.released_by,
    b.release_by_person, b.paid_on, b.paid_method, b.paid_reference, b.closed_reason, b.created_by, b.arrangement_id, b.policy_version_id"
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
        release_by_person: r.get("release_by_person"),
        paid_on: r.get("paid_on"),
        paid_method: r.get("paid_method"),
        paid_reference: r.get("paid_reference"),
        closed_reason: r.get("closed_reason"),
        created_by: r.get("created_by"),
        arrangement_id: r.get("arrangement_id"),
        policy_version_id: r.get("policy_version_id"),
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
    pub arrangement_id: Option<Uuid>,
    pub policy_version_id: Option<Uuid>,
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
                            amount_cents, memo, external_ref, created_by, received_at, arrangement_id, policy_version_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
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
    .bind(new.arrangement_id)
    .bind(new.policy_version_id)
    .execute(&mut **tx)
    .await?;
    for (i, line) in new.lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO bill_lines (id, org_id, bill_id, position, description, amount_cents, account, quantity_micro, unit, unit_price_cents)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(Uuid::new_v4())
        .bind(org_id)
        .bind(id)
        .bind(i as i32)
        .bind(&line.description)
        .bind(line.amount_cents)
        .bind(&line.account)
        .bind(line.quantity.as_deref().and_then(payvy_domain::invoice_evidence::quantity_micro))
        .bind(&line.unit)
        .bind(line.unit_price_cents)
        .execute(&mut **tx)
        .await?;
    }
    Ok(InsertBill::Inserted(id))
}

/// Locks the bill for the rest of the transaction, so two people acting on
/// it at once are serialized.
/// A bill read without locking its row: for work that must not hold
/// locks across a network call (the ledger worker's posts).
pub async fn get_bill_read(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Bill>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        bill_cols!(),
        " FROM bills b JOIN vendors v ON v.id = b.vendor_id WHERE b.id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(bill).transpose()
}

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
    invoice_key: Option<&str>,
    limit: i64,
) -> anyhow::Result<Vec<Bill>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        bill_cols!(),
        " FROM bills b JOIN vendors v ON v.id = b.vendor_id
         WHERE ($1::text IS NULL OR b.state = $1) AND ($2::uuid IS NULL OR b.vendor_id = $2)
           AND ($3::text IS NULL OR b.invoice_key = $3)
         ORDER BY b.received_at DESC, b.id LIMIT $4"
    ))
    .bind(state.map(State::as_str))
    .bind(vendor_id)
    .bind(invoice_key)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(bill).collect()
}

pub async fn bill_lines(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Vec<Line>> {
    let rows = sqlx::query("SELECT description, amount_cents, account, quantity_micro, unit, unit_price_cents FROM bill_lines WHERE bill_id = $1 ORDER BY position")
        .bind(bill_id)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let micro: Option<i64> = r.get("quantity_micro");
            Line {
                description: r.get("description"),
                amount_cents: r.get("amount_cents"),
                account: r.get("account"),
                quantity: micro.map(payvy_domain::invoice_evidence::format_micro),
                unit: r.get("unit"),
                unit_price_cents: r.get("unit_price_cents"),
            }
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

/// The ledger hashes the memo. A changed memo after posting is a conflict.
pub async fn set_bill_memo(tx: &mut Tx, id: Uuid, memo: Option<&str>) -> anyhow::Result<()> {
    sqlx::query("UPDATE bills SET memo = $2 WHERE id = $1")
        .bind(id)
        .bind(memo)
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

// ── Documents ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Document {
    pub id: Uuid,
    pub sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub filename: String,
    #[serde(skip)]
    pub storage_key: String,
    pub source: String,
    pub status: String,
    pub bill_id: Option<Uuid>,
    pub email_id: Option<Uuid>,
    pub trashed_reason: Option<String>,
    pub received_at: DateTime<Utc>,
    pub created_by: String,
    /// None: not counted yet. 0: the file can't be read as a PDF.
    pub page_count: Option<i32>,
    /// The document this one was split from. It shares that document's
    /// original file (`storage_key`, `sha256`) and shows only its own pages.
    pub derived_from: Option<Uuid>,
}

macro_rules! document_cols {
    () => {
        "id, sha256, content_type, size_bytes, filename, storage_key, source, status, bill_id, email_id, trashed_reason, received_at, created_by, page_count, derived_from"
    };
}

fn document(r: &PgRow) -> Document {
    Document {
        id: r.get("id"),
        sha256: r.get("sha256"),
        content_type: r.get("content_type"),
        size_bytes: r.get("size_bytes"),
        filename: r.get("filename"),
        storage_key: r.get("storage_key"),
        source: r.get("source"),
        status: r.get("status"),
        bill_id: r.get("bill_id"),
        email_id: r.get("email_id"),
        trashed_reason: r.get("trashed_reason"),
        received_at: r.get("received_at"),
        created_by: r.get("created_by"),
        page_count: r.get("page_count"),
        derived_from: r.get("derived_from"),
    }
}

pub struct NewDocument<'a> {
    pub sha256: &'a str,
    pub content_type: &'a str,
    pub size_bytes: i64,
    pub filename: &'a str,
    pub storage_key: &'a str,
    pub source: &'a str,
    /// Set exactly when `source` is "email".
    pub email_id: Option<Uuid>,
    pub received_at: DateTime<Utc>,
    pub created_by: &'a str,
    /// Counted on arrival: 0 when the file can't be read as a PDF.
    pub page_count: Option<i32>,
}

/// The file that arrived with this hash. Documents split from it share the
/// hash and aren't counted: they are pages of the same file, not a second copy.
pub async fn document_by_hash(tx: &mut Tx, sha256: &str) -> anyhow::Result<Option<Document>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        document_cols!(),
        " FROM documents WHERE sha256 = $1 AND derived_from IS NULL"
    ))
    .bind(sha256)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(document))
}

/// None if this organization already has the file (a concurrent upload won).
pub async fn insert_document(
    tx: &mut Tx,
    org_id: Uuid,
    new: &NewDocument<'_>,
) -> anyhow::Result<Option<Document>> {
    let row = sqlx::query(concat!(
        "INSERT INTO documents (id, org_id, sha256, content_type, size_bytes, filename, storage_key, source,
                                email_id, received_at, created_by, page_count)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         ON CONFLICT (org_id, sha256) WHERE derived_from IS NULL DO NOTHING RETURNING ",
        document_cols!()
    ))
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(new.sha256)
    .bind(new.content_type)
    .bind(new.size_bytes)
    .bind(new.filename)
    .bind(new.storage_key)
    .bind(new.source)
    .bind(new.email_id)
    .bind(new.received_at)
    .bind(new.created_by)
    .bind(new.page_count)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(document))
}

/// A new document holding some pages of `source`: the same original file,
/// in the inbox, received when the source was. Its pages are its first
/// arrangement, written by the caller.
pub async fn insert_derived_document(
    tx: &mut Tx,
    org_id: Uuid,
    source: &Document,
    filename: &str,
    created_by: &str,
    page_count: Option<i32>,
) -> anyhow::Result<Document> {
    let row = sqlx::query(concat!(
        "INSERT INTO documents (id, org_id, sha256, content_type, size_bytes, filename, storage_key, source,
                                email_id, received_at, created_by, page_count, derived_from)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING ",
        document_cols!()
    ))
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(&source.sha256)
    .bind(&source.content_type)
    .bind(source.size_bytes)
    .bind(filename)
    .bind(&source.storage_key)
    .bind(&source.source)
    .bind(source.email_id)
    .bind(source.received_at)
    .bind(created_by)
    .bind(page_count)
    .bind(source.id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(document(&row))
}

/// Documents split from this one, oldest first.
pub async fn derived_documents(tx: &mut Tx, source_id: Uuid) -> anyhow::Result<Vec<Document>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        document_cols!(),
        " FROM documents WHERE derived_from = $1 ORDER BY received_at, filename, id"
    ))
    .bind(source_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(document).collect())
}

/// Filenames by id, for naming where documents came from.
pub async fn document_filenames(tx: &mut Tx, ids: &[Uuid]) -> anyhow::Result<Vec<(Uuid, String)>> {
    let rows = sqlx::query("SELECT id, filename FROM documents WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get("id"), r.get("filename")))
        .collect())
}

/// Moves a document between the inbox and split (every page moved to
/// other documents). Never for filing or trashing, which have their own.
pub async fn set_split(tx: &mut Tx, id: Uuid, split: bool) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE documents SET status = CASE WHEN $2 THEN 'split' ELSE 'inbox' END
         WHERE id = $1 AND bill_id IS NULL",
    )
    .bind(id)
    .bind(split)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Locks the document for the rest of the transaction.
pub async fn get_document(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Document>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        document_cols!(),
        " FROM documents WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(document))
}

/// Oldest first, so the inbox reads in arrival order.
pub async fn list_documents(
    tx: &mut Tx,
    status: &str,
    limit: i64,
) -> anyhow::Result<Vec<Document>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        document_cols!(),
        " FROM documents WHERE status = $1 ORDER BY received_at, id LIMIT $2"
    ))
    .bind(status)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(document).collect())
}

pub async fn email_documents(tx: &mut Tx, email_id: Uuid) -> anyhow::Result<Vec<Document>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        document_cols!(),
        " FROM documents WHERE email_id = $1 ORDER BY filename, id"
    ))
    .bind(email_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(document).collect())
}

pub async fn bill_documents(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Vec<Document>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        document_cols!(),
        " FROM documents WHERE bill_id = $1 ORDER BY received_at"
    ))
    .bind(bill_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(document).collect())
}

/// Who uploaded the documents filed with these bills: (bill, actor).
pub async fn bill_uploaders(tx: &mut Tx, bill_ids: &[Uuid]) -> anyhow::Result<Vec<(Uuid, String)>> {
    let rows = sqlx::query(
        "SELECT DISTINCT bill_id, created_by FROM documents WHERE bill_id = ANY($1) ORDER BY 1, 2",
    )
    .bind(bill_ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get("bill_id"), r.get("created_by")))
        .collect())
}

// ── Approval rules ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRules {
    pub submitter_cannot_approve: bool,
    /// None until a person first changes a rule.
    pub updated_by: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// The organization's approval rules; all off when none were ever set.
pub async fn approval_rules(tx: &mut Tx) -> anyhow::Result<ApprovalRules> {
    let row = sqlx::query(
        "SELECT submitter_cannot_approve, updated_by, updated_at FROM approval_rules FOR SHARE",
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(match row {
        Some(r) => ApprovalRules {
            submitter_cannot_approve: r.get("submitter_cannot_approve"),
            updated_by: Some(r.get("updated_by")),
            updated_at: Some(r.get("updated_at")),
        },
        None => ApprovalRules {
            submitter_cannot_approve: false,
            updated_by: None,
            updated_at: None,
        },
    })
}

pub async fn set_approval_rules(
    tx: &mut Tx,
    org_id: Uuid,
    submitter_cannot_approve: bool,
    by: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO approval_rules (org_id, submitter_cannot_approve, updated_by, updated_at)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (org_id) DO UPDATE SET submitter_cannot_approve = $2, updated_by = $3, updated_at = $4",
    )
    .bind(org_id)
    .bind(submitter_cannot_approve)
    .bind(by)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn file_document(tx: &mut Tx, id: Uuid, bill_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("UPDATE documents SET status = 'filed', bill_id = $2 WHERE id = $1")
        .bind(id)
        .bind(bill_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn trash_document(tx: &mut Tx, id: Uuid, reason: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE documents SET status = 'trashed', trashed_reason = $2 WHERE id = $1")
        .bind(id)
        .bind(reason)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

// ── Page arrangements ───────────────────────────────────────────────────────

/// Records how many pages a document's file has, the first time it is read.
pub async fn set_page_count(tx: &mut Tx, id: Uuid, page_count: i32) -> anyhow::Result<()> {
    sqlx::query("UPDATE documents SET page_count = $2 WHERE id = $1 AND page_count IS NULL")
        .bind(id)
        .bind(page_count)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// One version of a document's page arrangement. Never updated.
#[derive(Debug, Clone, Serialize)]
pub struct Arrangement {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version: i32,
    pub pages: Vec<PageRef>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

macro_rules! arrangement_cols {
    () => {
        "id, document_id, version, pages, created_at, created_by"
    };
}

fn arrangement(r: &PgRow) -> anyhow::Result<Arrangement> {
    let pages: serde_json::Value = r.get("pages");
    Ok(Arrangement {
        id: r.get("id"),
        document_id: r.get("document_id"),
        version: r.get("version"),
        pages: serde_json::from_value(pages)?,
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
    })
}

/// The current arrangement of each of these documents; a document still in
/// its original order (version 0) is absent.
pub async fn latest_arrangements(
    tx: &mut Tx,
    document_ids: &[Uuid],
) -> anyhow::Result<Vec<Arrangement>> {
    let rows = sqlx::query(concat!(
        "SELECT DISTINCT ON (document_id) ",
        arrangement_cols!(),
        " FROM document_arrangements WHERE document_id = ANY($1) ORDER BY document_id, version DESC"
    ))
    .bind(document_ids)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(arrangement).collect()
}

/// Every version of a document's arrangement, newest first. Empty while it
/// is still the original (version 0).
pub async fn document_arrangements(
    tx: &mut Tx,
    document_id: Uuid,
) -> anyhow::Result<Vec<Arrangement>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        arrangement_cols!(),
        " FROM document_arrangements WHERE document_id = $1 ORDER BY version DESC"
    ))
    .bind(document_id)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(arrangement).collect()
}

pub async fn get_arrangement(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Arrangement>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        arrangement_cols!(),
        " FROM document_arrangements WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(arrangement).transpose()
}

pub async fn insert_arrangement(
    tx: &mut Tx,
    org_id: Uuid,
    document_id: Uuid,
    version: i32,
    pages: &[PageRef],
    at: DateTime<Utc>,
    by: &str,
) -> anyhow::Result<Arrangement> {
    let row = sqlx::query(concat!(
        "INSERT INTO document_arrangements (id, org_id, document_id, version, pages, created_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING ",
        arrangement_cols!()
    ))
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(document_id)
    .bind(version)
    .bind(serde_json::to_value(pages)?)
    .bind(at)
    .bind(by)
    .fetch_one(&mut **tx)
    .await?;
    arrangement(&row)
}

/// Moves a bill's pin to another arrangement of its document.
pub async fn pin_arrangement(
    tx: &mut Tx,
    bill_id: Uuid,
    arrangement_id: Uuid,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE bills SET arrangement_id = $2 WHERE id = $1")
        .bind(bill_id)
        .bind(arrangement_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

// ── Email ───────────────────────────────────────────────────────────────────

impl Store {
    /// The organization a live inbound token routes to. Outside any
    /// organization's transaction: the webhook doesn't know it yet.
    pub async fn org_for_inbound_token(&self, token: &str) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query(
            "SELECT org_id FROM inbound_addresses WHERE token = $1 AND retired_at IS NULL",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get("org_id")))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InboundAddress {
    pub token: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

// inbound_addresses has no row-level security, so these filter by org_id.
pub async fn live_inbound_address(
    tx: &mut Tx,
    org_id: Uuid,
) -> anyhow::Result<Option<InboundAddress>> {
    let row = sqlx::query(
        "SELECT token, created_at, created_by FROM inbound_addresses
         WHERE org_id = $1 AND retired_at IS NULL FOR UPDATE",
    )
    .bind(org_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| InboundAddress {
        token: r.get("token"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
    }))
}

/// Retires the organization's live address, if any, and makes `token` the
/// live one. False if the token is taken (a collision; pick another).
pub async fn replace_inbound_address(
    tx: &mut Tx,
    org_id: Uuid,
    token: &str,
    now: DateTime<Utc>,
    by: &str,
) -> anyhow::Result<bool> {
    sqlx::query(
        "UPDATE inbound_addresses SET retired_at = $2 WHERE org_id = $1 AND retired_at IS NULL",
    )
    .bind(org_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    let done = sqlx::query(
        "INSERT INTO inbound_addresses (token, org_id, created_at, created_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (token) DO NOTHING",
    )
    .bind(token)
    .bind(org_id)
    .bind(now)
    .bind(by)
    .execute(&mut **tx)
    .await?;
    Ok(done.rows_affected() == 1)
}

#[derive(Debug, Clone, Serialize)]
pub struct Email {
    pub id: Uuid,
    pub message_id: Option<String>,
    pub raw_sha256: String,
    pub size_bytes: i64,
    #[serde(skip)]
    pub storage_key: String,
    pub recipient: Option<String>,
    pub from_address: Option<String>,
    pub from_name: Option<String>,
    pub subject: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub authentication_results: Option<String>,
    pub skipped: serde_json::Value,
    pub received_at: DateTime<Utc>,
    pub created_by: String,
}

macro_rules! email_cols {
    () => {
        "id, message_id, raw_sha256, size_bytes, storage_key, recipient, from_address, from_name, subject, sent_at, authentication_results, skipped, received_at, created_by"
    };
}

fn email(r: &PgRow) -> Email {
    Email {
        id: r.get("id"),
        message_id: r.get("message_id"),
        raw_sha256: r.get("raw_sha256"),
        size_bytes: r.get("size_bytes"),
        storage_key: r.get("storage_key"),
        recipient: r.get("recipient"),
        from_address: r.get("from_address"),
        from_name: r.get("from_name"),
        subject: r.get("subject"),
        sent_at: r.get("sent_at"),
        authentication_results: r.get("authentication_results"),
        skipped: r.get("skipped"),
        received_at: r.get("received_at"),
        created_by: r.get("created_by"),
    }
}

pub struct NewEmail<'a> {
    pub id: Uuid,
    pub message_id: Option<&'a str>,
    pub raw_sha256: &'a str,
    pub size_bytes: i64,
    pub storage_key: &'a str,
    pub recipient: Option<&'a str>,
    pub from_address: Option<&'a str>,
    pub from_name: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub sent_at: Option<DateTime<Utc>>,
    pub authentication_results: Option<&'a str>,
    pub received_at: DateTime<Utc>,
    pub created_by: &'a str,
}

/// The email already here with this Message-ID or these exact bytes.
pub async fn existing_email(
    tx: &mut Tx,
    message_id: Option<&str>,
    raw_sha256: &str,
) -> anyhow::Result<Option<Email>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        email_cols!(),
        " FROM emails WHERE raw_sha256 = $2 OR ($1::text IS NOT NULL AND message_id = $1) LIMIT 1"
    ))
    .bind(message_id)
    .bind(raw_sha256)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(email))
}

/// False if the email is already here (a concurrent delivery won).
pub async fn insert_email(tx: &mut Tx, org_id: Uuid, new: &NewEmail<'_>) -> anyhow::Result<bool> {
    let done = sqlx::query(
        "INSERT INTO emails (id, org_id, message_id, raw_sha256, size_bytes, storage_key, recipient,
                             from_address, from_name, subject, sent_at, authentication_results,
                             received_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         ON CONFLICT DO NOTHING",
    )
    .bind(new.id)
    .bind(org_id)
    .bind(new.message_id)
    .bind(new.raw_sha256)
    .bind(new.size_bytes)
    .bind(new.storage_key)
    .bind(new.recipient)
    .bind(new.from_address)
    .bind(new.from_name)
    .bind(new.subject)
    .bind(new.sent_at)
    .bind(new.authentication_results)
    .bind(new.received_at)
    .bind(new.created_by)
    .execute(&mut **tx)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn set_email_skipped(
    tx: &mut Tx,
    id: Uuid,
    skipped: &serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE emails SET skipped = $2 WHERE id = $1")
        .bind(id)
        .bind(skipped)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn get_email(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Email>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        email_cols!(),
        " FROM emails WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(email))
}

/// Newest first.
pub async fn list_emails(tx: &mut Tx, limit: i64) -> anyhow::Result<Vec<Email>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        email_cols!(),
        " FROM emails ORDER BY received_at DESC, id LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(email).collect())
}

// ── Extractions ─────────────────────────────────────────────────────────────

impl Store {
    /// Every organization, for background work that runs across them (each
    /// organization's own work still runs in its own transaction).
    pub async fn org_ids(&self) -> anyhow::Result<Vec<Uuid>> {
        let rows = sqlx::query("SELECT id FROM organizations ORDER BY id")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get("id")).collect())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Extraction {
    pub analysis: Option<serde_json::Value>,
    pub id: Uuid,
    pub document_id: Uuid,
    /// read or failed
    pub status: String,
    pub provider: String,
    pub model: String,
    /// What the model returned, as printed on the document.
    pub answer: Option<serde_json::Value>,
    pub draft: Option<serde_json::Value>,
    pub checks: Option<serde_json::Value>,
    pub vendor_match: Option<serde_json::Value>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    /// The document's page arrangement version when it was read.
    pub arrangement_version: i32,
    /// The invoices it found, when the file holds several and their pages
    /// checked out.
    pub invoices: Option<serde_json::Value>,
}

macro_rules! extraction_cols {
    () => {
        "id, document_id, status, provider, model, answer, draft, checks, vendor_match, error, created_at, created_by, arrangement_version, invoices, analysis"
    };
}

fn extraction(r: &PgRow) -> Extraction {
    Extraction {
        analysis: r.get("analysis"),
        id: r.get("id"),
        document_id: r.get("document_id"),
        status: r.get("status"),
        provider: r.get("provider"),
        model: r.get("model"),
        answer: r.get("answer"),
        draft: r.get("draft"),
        checks: r.get("checks"),
        vendor_match: r.get("vendor_match"),
        error: r.get("error"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
        arrangement_version: r.get("arrangement_version"),
        invoices: r.get("invoices"),
    }
}

pub struct NewExtraction<'a> {
    pub document_id: Uuid,
    pub provider: &'a str,
    pub model: &'a str,
    /// Ok: the answer, draft, checks and vendor match. Err: what went wrong.
    pub outcome: Result<ExtractionRead<'a>, &'a str>,
    pub created_at: DateTime<Utc>,
    pub created_by: &'a str,
    pub arrangement_version: i32,
}

pub struct ExtractionRead<'a> {
    pub answer: &'a serde_json::Value,
    pub draft: &'a serde_json::Value,
    pub checks: &'a serde_json::Value,
    pub vendor_match: &'a serde_json::Value,
    /// Several invoices found in the file; None for one.
    pub invoices: Option<&'a serde_json::Value>,
}

pub async fn insert_extraction(
    tx: &mut Tx,
    org_id: Uuid,
    new: &NewExtraction<'_>,
) -> anyhow::Result<Extraction> {
    let (status, read, error) = match &new.outcome {
        Ok(r) => ("read", Some(r), None),
        Err(e) => ("failed", None, Some(*e)),
    };
    let row = sqlx::query(concat!(
        "INSERT INTO extractions (id, org_id, document_id, status, provider, model, answer, draft, checks,
                                  vendor_match, error, created_at, created_by, arrangement_version, invoices)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING ",
        extraction_cols!()
    ))
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(new.document_id)
    .bind(status)
    .bind(new.provider)
    .bind(new.model)
    .bind(read.map(|r| r.answer))
    .bind(read.map(|r| r.draft))
    .bind(read.map(|r| r.checks))
    .bind(read.map(|r| r.vendor_match))
    .bind(error)
    .bind(new.created_at)
    .bind(new.created_by)
    .bind(new.arrangement_version)
    .bind(read.and_then(|r| r.invoices))
    .fetch_one(&mut **tx)
    .await?;
    Ok(extraction(&row))
}

pub async fn get_extraction(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Extraction>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        extraction_cols!(),
        " FROM extractions WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(extraction))
}

/// Newest first.
pub async fn document_extractions(
    tx: &mut Tx,
    document_id: Uuid,
) -> anyhow::Result<Vec<Extraction>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        extraction_cols!(),
        " FROM extractions WHERE document_id = $1 ORDER BY created_at DESC, id"
    ))
    .bind(document_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(extraction).collect())
}

/// The newest reading of each of these documents (documents never read are
/// absent), for lists that show what was read at a glance.
pub async fn latest_extractions(
    tx: &mut Tx,
    document_ids: &[Uuid],
) -> anyhow::Result<Vec<Extraction>> {
    let rows = sqlx::query(concat!(
        "SELECT DISTINCT ON (document_id) ",
        extraction_cols!(),
        " FROM extractions WHERE document_id = ANY($1) ORDER BY document_id, created_at DESC, id"
    ))
    .bind(document_ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(extraction).collect())
}

/// Inbox documents no model has read yet, oldest first. A document that
/// failed `max_failures` times is left for a person; after a failure it
/// waits `retry_after` before the next try.
pub async fn documents_to_extract(
    tx: &mut Tx,
    now: DateTime<Utc>,
    retry_after: chrono::Duration,
    max_failures: i64,
    limit: i64,
) -> anyhow::Result<Vec<Uuid>> {
    let rows = sqlx::query(
        "SELECT d.id FROM documents d
         LEFT JOIN LATERAL (
             SELECT count(*) FILTER (WHERE e.status = 'read') AS reads,
                    count(*) FILTER (WHERE e.status = 'failed') AS failures,
                    max(e.created_at) AS last
             FROM extractions e WHERE e.document_id = d.id
         ) x ON true
         WHERE d.status = 'inbox' AND x.reads = 0 AND x.failures < $3
           AND (x.last IS NULL OR x.last < $1 - $2)
         ORDER BY d.received_at, d.id LIMIT $4",
    )
    .bind(now)
    .bind(retry_after)
    .bind(max_failures)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(|r| r.get("id")).collect())
}

/// Every vendor with its email and account numbers, for matching.
pub async fn vendor_candidates(
    tx: &mut Tx,
) -> anyhow::Result<Vec<payvy_domain::extraction::Candidate<Uuid>>> {
    let rows = sqlx::query(
        "SELECT v.id, v.name, v.email,
                coalesce(array_agg(a.account_number) FILTER (WHERE a.id IS NOT NULL), '{}') AS accounts
         FROM vendors v LEFT JOIN vendor_accounts a ON a.vendor_id = v.id
         GROUP BY v.id ORDER BY v.id",
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .iter()
        .map(|r| payvy_domain::extraction::Candidate {
            id: r.get("id"),
            name: r.get("name"),
            email: r.get("email"),
            account_numbers: r.get("accounts"),
        })
        .collect())
}

/// A live bill from this vendor with this invoice key.
pub async fn bill_with_invoice_key(
    tx: &mut Tx,
    vendor_id: Uuid,
    invoice_key: &str,
) -> anyhow::Result<Option<Uuid>> {
    let row = sqlx::query(
        "SELECT id FROM bills WHERE vendor_id = $1 AND invoice_key = $2 AND state <> 'void'",
    )
    .bind(vendor_id)
    .bind(invoice_key)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| r.get("id")))
}

// ── Paying through the customer's bank ──────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct BankConnection {
    pub id: Uuid,
    pub provider: String,
    pub environment: String,
    pub mode: String,
    pub account_id: String,
    pub account_name: String,
    #[serde(skip)]
    pub token_sealed: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub retired_at: Option<DateTime<Utc>>,
    /// Mercury's id for the webhook Payvy registered, if it has one.
    pub webhook_id: Option<String>,
    #[serde(skip)]
    pub webhook_secret_sealed: Option<Vec<u8>>,
}

macro_rules! connection_cols {
    () => {
        "id, provider, environment, mode, account_id, account_name, token_sealed, created_at, created_by, retired_at,
         webhook_id, webhook_secret_sealed"
    };
}

fn connection(r: &PgRow) -> BankConnection {
    BankConnection {
        id: r.get("id"),
        provider: r.get("provider"),
        environment: r.get("environment"),
        mode: r.get("mode"),
        account_id: r.get("account_id"),
        account_name: r.get("account_name"),
        token_sealed: r.get("token_sealed"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
        retired_at: r.get("retired_at"),
        webhook_id: r.get("webhook_id"),
        webhook_secret_sealed: r.get("webhook_secret_sealed"),
    }
}

pub async fn set_connection_webhook(
    tx: &mut Tx,
    id: Uuid,
    webhook_id: &str,
    secret_sealed: &[u8],
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE bank_connections SET webhook_id = $2, webhook_secret_sealed = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(webhook_id)
    .bind(secret_sealed)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Payments a Mercury transaction event may be about: the one with that
/// transaction, and any on the connection still waiting for theirs (an
/// approved request becomes a transaction Payvy hasn't seen yet).
pub async fn payments_for_transaction(
    tx: &mut Tx,
    connection_id: Uuid,
    transaction_id: &str,
) -> anyhow::Result<Vec<Uuid>> {
    let rows = sqlx::query(
        "SELECT id FROM payments WHERE connection_id = $1
           AND (transaction_id = $2
                OR (transaction_id IS NULL
                    AND status IN ('submitting', 'awaiting_bank_approval', 'pending')))
         ORDER BY created_at",
    )
    .bind(connection_id)
    .bind(transaction_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(|r| r.get("id")).collect())
}

pub struct NewConnection<'a> {
    pub id: Uuid,
    pub provider: &'a str,
    pub environment: &'a str,
    pub mode: &'a str,
    pub account_id: &'a str,
    pub account_name: &'a str,
    pub token_sealed: &'a [u8],
    pub created_at: DateTime<Utc>,
    pub created_by: &'a str,
}

/// The live paying connection (Mercury), if any. Read-only connections
/// (Rho) never pay, so they don't count here.
pub async fn live_paying_connection(tx: &mut Tx) -> anyhow::Result<Option<BankConnection>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        connection_cols!(),
        " FROM bank_connections WHERE retired_at IS NULL AND mode <> 'read_only'"
    ))
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(connection))
}

/// Every live connection, paying and read-only, oldest first.
pub async fn live_connections(tx: &mut Tx) -> anyhow::Result<Vec<BankConnection>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        connection_cols!(),
        " FROM bank_connections WHERE retired_at IS NULL ORDER BY created_at, id"
    ))
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(connection).collect())
}

/// The live read-only connection for a provider (e.g. Rho), if any.
pub async fn live_read_connection(
    tx: &mut Tx,
    provider: &str,
) -> anyhow::Result<Option<BankConnection>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        connection_cols!(),
        " FROM bank_connections WHERE retired_at IS NULL AND mode = 'read_only' AND provider = $1"
    ))
    .bind(provider)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(connection))
}

pub async fn get_connection(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<BankConnection>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        connection_cols!(),
        " FROM bank_connections WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(connection))
}

/// Retires the live paying connection, if any. True if there was one.
pub async fn retire_paying_connection(
    tx: &mut Tx,
    at: DateTime<Utc>,
    by: &str,
) -> anyhow::Result<bool> {
    let done = sqlx::query(
        "UPDATE bank_connections SET retired_at = $1, retired_by = $2 WHERE retired_at IS NULL AND mode <> 'read_only'",
    )
    .bind(at)
    .bind(by)
    .execute(&mut **tx)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Retires one live connection by id, if it is still live.
pub async fn retire_connection_by_id(
    tx: &mut Tx,
    id: Uuid,
    at: DateTime<Utc>,
    by: &str,
) -> anyhow::Result<bool> {
    let done = sqlx::query(
        "UPDATE bank_connections SET retired_at = $1, retired_by = $2 WHERE retired_at IS NULL AND id = $3",
    )
    .bind(at)
    .bind(by)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(done.rows_affected() > 0)
}

pub async fn insert_connection(
    tx: &mut Tx,
    org_id: Uuid,
    new: &NewConnection<'_>,
) -> anyhow::Result<BankConnection> {
    let row = sqlx::query(concat!(
        "INSERT INTO bank_connections (id, org_id, provider, environment, mode, account_id, account_name,
                                       token_sealed, created_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING ",
        connection_cols!()
    ))
    .bind(new.id)
    .bind(org_id)
    .bind(new.provider)
    .bind(new.environment)
    .bind(new.mode)
    .bind(new.account_id)
    .bind(new.account_name)
    .bind(new.token_sealed)
    .bind(new.created_at)
    .bind(new.created_by)
    .fetch_one(&mut **tx)
    .await?;
    Ok(connection(&row))
}

#[derive(Debug, Clone, Serialize)]
pub struct Payee {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub connection_id: Uuid,
    pub recipient_id: String,
    pub recipient_name: String,
    pub instrument_id: Option<Uuid>,
    pub instrument_fingerprint: Option<String>,
    pub method: String,
    pub destination: String,
    pub status: String,
    pub reason: Option<String>,
    pub proposed_by: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

macro_rules! payee_cols {
    () => {
        "id, vendor_id, connection_id, recipient_id, recipient_name, instrument_id, instrument_fingerprint, method, destination, status, reason, proposed_by, decided_by, decided_at, created_at"
    };
}

fn payee(r: &PgRow) -> Payee {
    Payee {
        id: r.get("id"),
        vendor_id: r.get("vendor_id"),
        connection_id: r.get("connection_id"),
        recipient_id: r.get("recipient_id"),
        recipient_name: r.get("recipient_name"),
        instrument_id: r.get("instrument_id"),
        instrument_fingerprint: r.get("instrument_fingerprint"),
        method: r.get("method"),
        destination: r.get("destination"),
        status: r.get("status"),
        reason: r.get("reason"),
        proposed_by: r.get("proposed_by"),
        decided_by: r.get("decided_by"),
        decided_at: r.get("decided_at"),
        created_at: r.get("created_at"),
    }
}

pub struct NewPayee<'a> {
    pub vendor_id: Uuid,
    pub connection_id: Uuid,
    pub recipient_id: &'a str,
    pub recipient_name: &'a str,
    pub method: &'a str,
    pub destination: &'a str,
    pub reason: Option<&'a str>,
    pub proposed_by: &'a str,
    pub created_at: DateTime<Utc>,
}

pub async fn insert_payee_proposal(
    tx: &mut Tx,
    org_id: Uuid,
    new: &NewPayee<'_>,
) -> anyhow::Result<Payee> {
    let row = sqlx::query(concat!(
        "INSERT INTO vendor_payees (id, org_id, vendor_id, connection_id, recipient_id, recipient_name, method,
                                    destination, status, reason, proposed_by, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'proposed', $9, $10, $11) RETURNING ",
        payee_cols!()
    ))
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(new.vendor_id)
    .bind(new.connection_id)
    .bind(new.recipient_id)
    .bind(new.recipient_name)
    .bind(new.method)
    .bind(new.destination)
    .bind(new.reason)
    .bind(new.proposed_by)
    .bind(new.created_at)
    .fetch_one(&mut **tx)
    .await?;
    Ok(payee(&row))
}

/// Locks the payee for the rest of the transaction.
pub async fn get_payee(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Payee>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        payee_cols!(),
        " FROM vendor_payees WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(payee))
}

/// The active payee and open proposals, active first.
pub async fn vendor_payees(tx: &mut Tx, vendor_id: Uuid) -> anyhow::Result<Vec<Payee>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        payee_cols!(),
        " FROM vendor_payees WHERE vendor_id = $1 AND status IN ('active', 'proposed')
          ORDER BY status = 'active' DESC, created_at"
    ))
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(payee).collect())
}

pub async fn active_payee(tx: &mut Tx, vendor_id: Uuid) -> anyhow::Result<Option<Payee>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        payee_cols!(),
        " FROM vendor_payees WHERE vendor_id = $1 AND status = 'active'"
    ))
    .bind(vendor_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(payee))
}

/// Accepts (superseding the vendor's active payee) or declines a proposal.
pub async fn decide_payee(
    tx: &mut Tx,
    id: Uuid,
    vendor_id: Uuid,
    accept: bool,
    by: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    if accept {
        sqlx::query(
            "UPDATE vendor_payees SET status = 'superseded' WHERE vendor_id = $1 AND status = 'active'",
        )
        .bind(vendor_id)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "UPDATE vendor_payees SET status = $2, decided_by = $3, decided_at = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(if accept { "active" } else { "declined" })
    .bind(by)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct PaymentRow {
    pub id: Uuid,
    pub bill_id: Uuid,
    pub connection_id: Uuid,
    pub payee_id: Uuid,
    pub amount_cents: Cents,
    pub method: String,
    pub idempotency_key: String,
    pub status: String,
    pub request_id: Option<String>,
    pub transaction_id: Option<String>,
    pub failure: Option<String>,
    pub link: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub checked_at: DateTime<Utc>,
}

macro_rules! payment_cols {
    () => {
        "id, bill_id, connection_id, payee_id, amount_cents, method, idempotency_key, status, request_id, transaction_id, failure, link, created_at, created_by, updated_at, checked_at"
    };
}

fn payment(r: &PgRow) -> PaymentRow {
    PaymentRow {
        id: r.get("id"),
        bill_id: r.get("bill_id"),
        connection_id: r.get("connection_id"),
        payee_id: r.get("payee_id"),
        amount_cents: r.get("amount_cents"),
        method: r.get("method"),
        idempotency_key: r.get("idempotency_key"),
        status: r.get("status"),
        request_id: r.get("request_id"),
        transaction_id: r.get("transaction_id"),
        failure: r.get("failure"),
        link: r.get("link"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
        updated_at: r.get("updated_at"),
        checked_at: r.get("checked_at"),
    }
}

pub struct NewPayment<'a> {
    pub id: Uuid,
    pub bill_id: Uuid,
    pub connection_id: Uuid,
    pub payee_id: Uuid,
    pub amount_cents: Cents,
    pub method: &'a str,
    pub idempotency_key: &'a str,
    pub created_at: DateTime<Utc>,
    pub created_by: &'a str,
}

/// Starts as `submitting`; the caller records what the bank said.
pub async fn insert_payment(
    tx: &mut Tx,
    org_id: Uuid,
    new: &NewPayment<'_>,
) -> anyhow::Result<PaymentRow> {
    let row = sqlx::query(concat!(
        "INSERT INTO payments (id, org_id, bill_id, connection_id, payee_id, amount_cents, method,
                               idempotency_key, status, created_at, created_by, updated_at, checked_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'submitting', $9, $10, $9, $9) RETURNING ",
        payment_cols!()
    ))
    .bind(new.id)
    .bind(org_id)
    .bind(new.bill_id)
    .bind(new.connection_id)
    .bind(new.payee_id)
    .bind(new.amount_cents)
    .bind(new.method)
    .bind(new.idempotency_key)
    .bind(new.created_at)
    .bind(new.created_by)
    .fetch_one(&mut **tx)
    .await?;
    Ok(payment(&row))
}

/// Locks the payment for the rest of the transaction.
pub async fn get_payment(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<PaymentRow>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        payment_cols!(),
        " FROM payments WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(payment))
}

pub struct PaymentUpdate<'a> {
    pub status: &'a str,
    pub request_id: Option<&'a str>,
    pub transaction_id: Option<&'a str>,
    pub failure: Option<&'a str>,
    pub link: Option<&'a str>,
}

/// Records what the bank said. Ids and links, once known, are kept.
pub async fn update_payment(
    tx: &mut Tx,
    id: Uuid,
    u: &PaymentUpdate<'_>,
    at: DateTime<Utc>,
) -> anyhow::Result<PaymentRow> {
    let row = sqlx::query(concat!(
        "UPDATE payments SET
             updated_at = CASE WHEN status IS DISTINCT FROM $2 THEN $7 ELSE updated_at END,
             status = $2,
             request_id = coalesce($3, request_id),
             transaction_id = coalesce($4, transaction_id),
             failure = $5,
             link = coalesce($6, link),
             checked_at = $7
         WHERE id = $1 RETURNING ",
        payment_cols!()
    ))
    .bind(id)
    .bind(u.status)
    .bind(u.request_id)
    .bind(u.transaction_id)
    .bind(u.failure)
    .bind(u.link)
    .bind(at)
    .fetch_one(&mut **tx)
    .await?;
    Ok(payment(&row))
}

pub async fn touch_payment(tx: &mut Tx, id: Uuid, at: DateTime<Utc>) -> anyhow::Result<()> {
    sqlx::query("UPDATE payments SET checked_at = $2 WHERE id = $1")
        .bind(id)
        .bind(at)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Newest first.
pub async fn bill_payments(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Vec<PaymentRow>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        payment_cols!(),
        " FROM payments WHERE bill_id = $1 ORDER BY seq DESC"
    ))
    .bind(bill_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(payment).collect())
}

/// Newest first; `open_only` keeps those still going.
pub async fn list_payments(
    tx: &mut Tx,
    open_only: bool,
    limit: i64,
) -> anyhow::Result<Vec<PaymentRow>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        payment_cols!(),
        " FROM payments WHERE NOT $1 OR status IN ('submitting', 'awaiting_bank_approval', 'pending')
          ORDER BY seq DESC LIMIT $2"
    ))
    .bind(open_only)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(payment).collect())
}

/// Payments to ask the bank about: those still going, and sent ones young
/// enough to come back as an ACH return.
pub async fn payments_to_check(
    tx: &mut Tx,
    now: DateTime<Utc>,
    every: chrono::Duration,
    watch_sent_for: chrono::Duration,
    limit: i64,
) -> anyhow::Result<Vec<Uuid>> {
    let rows = sqlx::query(
        "SELECT id FROM payments
         WHERE checked_at < $1 - $2
           AND (status IN ('submitting', 'awaiting_bank_approval', 'pending')
                OR (status = 'sent' AND updated_at > $1 - $3))
         ORDER BY checked_at LIMIT $4",
    )
    .bind(now)
    .bind(every)
    .bind(watch_sent_for)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(|r| r.get("id")).collect())
}

/// Takes back a release whose payment didn't go, so the bill waits for a
/// person again.
pub async fn unrelease_bill(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("UPDATE bills SET released_at = NULL, released_by = NULL, release_by_person = true WHERE id = $1")
        .bind(bill_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// A paid bill whose payment came back is open again, waiting for release.
pub async fn reopen_returned_bill(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE bills SET state = 'open', paid_on = NULL, paid_method = NULL, paid_reference = NULL,
                          released_at = NULL, released_by = NULL, release_by_person = true
         WHERE id = $1",
    )
    .bind(bill_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ── People and sign-in ──────────────────────────────────────────────────────
//
// No row-level security on these tables (see 0006_people.sql), so every query
// names the organization or starts from a token that does.

#[derive(Debug, Clone, Serialize)]
pub struct Person {
    pub id: Uuid,
    pub org_id: Uuid,
    pub actor: String,
    pub email: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

macro_rules! person_cols {
    () => {
        "p.id, p.org_id, p.actor, p.email, p.name, p.created_at, p.created_by"
    };
}

fn person_from(r: &PgRow) -> Person {
    Person {
        id: r.get("id"),
        org_id: r.get("org_id"),
        actor: r.get("actor"),
        email: r.get("email"),
        name: r.get("name"),
        created_at: r.get("created_at"),
        created_by: r.get("created_by"),
    }
}

/// The person and the organization's name.
#[derive(Debug, Clone, Serialize)]
pub struct SignedIn {
    pub person: Person,
    pub org_name: String,
}

pub struct NewPerson<'a> {
    pub org_id: Uuid,
    pub actor: &'a str,
    pub email: &'a str,
    pub name: &'a str,
    pub created_at: DateTime<Utc>,
    pub created_by: &'a str,
}

/// Adds a person. None when the actor or email is already taken.
pub async fn insert_person(tx: &mut Tx, p: &NewPerson<'_>) -> anyhow::Result<Option<Person>> {
    let id = Uuid::new_v4();
    let r = sqlx::query(
        "INSERT INTO people (id, org_id, actor, email, name, created_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(p.org_id)
    .bind(p.actor)
    .bind(p.email)
    .bind(p.name)
    .bind(p.created_at)
    .bind(p.created_by)
    .execute(&mut **tx)
    .await?;
    if r.rows_affected() == 0 {
        return Ok(None);
    }
    set_actor_roles(tx, p.org_id, p.actor, &[Role::ApClerk]).await?;
    Ok(Some(Person {
        id,
        org_id: p.org_id,
        actor: p.actor.to_owned(),
        email: p.email.to_owned(),
        name: p.name.to_owned(),
        created_at: p.created_at,
        created_by: p.created_by.to_owned(),
    }))
}

/// Everyone in an organization who can sign in.
pub async fn people(tx: &mut Tx, org_id: Uuid) -> anyhow::Result<Vec<Person>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        person_cols!(),
        " FROM people p WHERE p.org_id = $1 AND p.removed_at IS NULL ORDER BY p.name, p.actor"
    ))
    .bind(org_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(person_from).collect())
}

/// Removes a person and ends their sessions. None if there was no such person.
pub async fn remove_person(
    tx: &mut Tx,
    org_id: Uuid,
    actor: &str,
    at: DateTime<Utc>,
    by: &str,
) -> anyhow::Result<Option<Uuid>> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "UPDATE people SET removed_at = $3, removed_by = $4
         WHERE org_id = $1 AND actor = $2 AND removed_at IS NULL RETURNING id",
    )
    .bind(org_id)
    .bind(actor)
    .bind(at)
    .bind(by)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(id) = id {
        sqlx::query("DELETE FROM actor_roles WHERE org_id = $1 AND actor = $2")
            .bind(org_id)
            .bind(actor)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE api_keys SET revoked_at = $3 WHERE org_id = $1 AND actor = $2 AND kind = 'person' AND revoked_at IS NULL").bind(org_id).bind(actor).bind(at).execute(&mut **tx).await?;
        sqlx::query("UPDATE sessions SET ended_at = $2 WHERE person_id = $1 AND ended_at IS NULL")
            .bind(id)
            .bind(at)
            .execute(&mut **tx)
            .await?;
    }
    Ok(id)
}

/// Actors of everyone who can sign in, locked until the transaction ends so
/// two removals at once can't leave nobody.
pub async fn lock_people(tx: &mut Tx, org_id: Uuid) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT actor FROM people WHERE org_id = $1 AND removed_at IS NULL ORDER BY actor FOR UPDATE",
    )
    .bind(org_id)
    .fetch_all(&mut **tx)
    .await?)
}

#[derive(Debug, Clone, Serialize)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

fn organization_from(r: &PgRow) -> Organization {
    Organization {
        id: r.get("id"),
        name: r.get("name"),
        created_at: r.get("created_at"),
    }
}

/// The organization itself. Like people, it has no row-level security (it is
/// what the policy names), so the query names it.
pub async fn organization(tx: &mut Tx, org_id: Uuid) -> anyhow::Result<Option<Organization>> {
    let row = sqlx::query("SELECT id, name, created_at FROM organizations WHERE id = $1")
        .bind(org_id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.as_ref().map(organization_from))
}

/// The organization, its row locked until the transaction ends.
pub async fn lock_organization(tx: &mut Tx, org_id: Uuid) -> anyhow::Result<Option<Organization>> {
    let row =
        sqlx::query("SELECT id, name, created_at FROM organizations WHERE id = $1 FOR UPDATE")
            .bind(org_id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(row.as_ref().map(organization_from))
}

pub async fn rename_organization(
    tx: &mut Tx,
    org_id: Uuid,
    name: &str,
) -> anyhow::Result<Organization> {
    let row = sqlx::query(
        "UPDATE organizations SET name = $2 WHERE id = $1 RETURNING id, name, created_at",
    )
    .bind(org_id)
    .bind(name)
    .fetch_one(&mut **tx)
    .await?;
    Ok(organization_from(&row))
}

impl Store {
    /// Every organization this email can sign in to.
    pub async fn people_by_email(&self, email: &str) -> anyhow::Result<Vec<SignedIn>> {
        let rows = sqlx::query(concat!(
            "SELECT ",
            person_cols!(),
            ", o.name AS org_name FROM people p
             JOIN organizations o ON o.id = p.org_id
             WHERE p.email = $1 AND p.removed_at IS NULL ORDER BY o.name"
        ))
        .bind(email)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| SignedIn {
                person: person_from(r),
                org_name: r.get("org_name"),
            })
            .collect())
    }

    /// Sign-in links made for this person since a time, used or not.
    pub async fn recent_sign_in_links(
        &self,
        person_id: Uuid,
        since: DateTime<Utc>,
    ) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM sign_in_links WHERE person_id = $1 AND created_at >= $2",
        )
        .bind(person_id)
        .bind(since)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn insert_sign_in_link(
        &self,
        token_hash: &str,
        person_id: Uuid,
        at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO sign_in_links (token_hash, person_id, created_at, expires_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(token_hash)
        .bind(person_id)
        .bind(at)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Uses a sign-in link and starts a session, in one transaction. None if
    /// the link is unknown, used, expired, or its person was removed.
    pub async fn redeem_sign_in_link(
        &self,
        link_hash: &str,
        session_hash: &str,
        at: DateTime<Utc>,
        session_expires_at: DateTime<Utc>,
    ) -> anyhow::Result<Option<SignedIn>> {
        let mut tx = self.pool.begin().await?;
        let person_id: Option<Uuid> = sqlx::query_scalar(
            "UPDATE sign_in_links l SET used_at = $2
             FROM people p
             WHERE l.token_hash = $1 AND l.used_at IS NULL AND l.expires_at > $2
               AND p.id = l.person_id AND p.removed_at IS NULL
             RETURNING l.person_id",
        )
        .bind(link_hash)
        .bind(at)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(person_id) = person_id else {
            return Ok(None);
        };
        sqlx::query(
            "INSERT INTO sessions (token_hash, person_id, created_at, expires_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(session_hash)
        .bind(person_id)
        .bind(at)
        .bind(session_expires_at)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(concat!(
            "SELECT ",
            person_cols!(),
            ", o.name AS org_name FROM people p
             JOIN organizations o ON o.id = p.org_id WHERE p.id = $1"
        ))
        .bind(person_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(SignedIn {
            person: person_from(&row),
            org_name: row.get("org_name"),
        }))
    }

    /// Who a session cookie belongs to; None if unknown, ended, expired, or
    /// the person was removed.
    pub async fn session(
        &self,
        session_hash: &str,
        at: DateTime<Utc>,
    ) -> anyhow::Result<Option<SignedIn>> {
        let row = sqlx::query(concat!(
            "SELECT ",
            person_cols!(),
            ", o.name AS org_name FROM sessions s
             JOIN people p ON p.id = s.person_id
             JOIN organizations o ON o.id = p.org_id
             WHERE s.token_hash = $1 AND s.ended_at IS NULL AND s.expires_at > $2
               AND p.removed_at IS NULL"
        ))
        .bind(session_hash)
        .bind(at)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| SignedIn {
            person: person_from(&r),
            org_name: r.get("org_name"),
        }))
    }

    pub async fn end_session(&self, session_hash: &str, at: DateTime<Utc>) -> anyhow::Result<()> {
        sqlx::query("UPDATE sessions SET ended_at = $2 WHERE token_hash = $1 AND ended_at IS NULL")
            .bind(session_hash)
            .bind(at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Payee proposals waiting for a person, oldest first.
pub async fn open_payee_proposals(tx: &mut Tx) -> anyhow::Result<Vec<Payee>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        payee_cols!(),
        " FROM vendor_payees WHERE status = 'proposed' ORDER BY created_at"
    ))
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(payee).collect())
}

/// Workflow proposals waiting for a person, for any vendor or the default,
/// oldest first.
pub async fn open_workflow_proposals(tx: &mut Tx) -> anyhow::Result<Vec<WorkflowRow>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        workflow_cols!(),
        " FROM workflows WHERE status = 'proposed' ORDER BY created_at"
    ))
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(workflow_row).collect()
}

// Recipient invitations contain no bank details or onboarding links.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PayeeInvite {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub connection_id: Uuid,
    pub contact_email: String,
    pub method: String,
    pub status: String,
    pub mercury_invite_id: Option<String>,
    pub proposal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

pub async fn vendor_payee_invites(
    tx: &mut Tx,
    vendor_id: Uuid,
) -> anyhow::Result<Vec<PayeeInvite>> {
    Ok(sqlx::query_as::<_, PayeeInvite>(
        "SELECT * FROM vendor_payee_invites WHERE vendor_id = $1 ORDER BY created_at DESC, id",
    )
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?)
}

pub async fn get_payee_invite(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<PayeeInvite>> {
    Ok(sqlx::query_as::<_, PayeeInvite>(
        "SELECT * FROM vendor_payee_invites WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?)
}

pub async fn insert_payee_invite(
    tx: &mut Tx,
    org_id: Uuid,
    invite: &PayeeInvite,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO vendor_payee_invites (id, org_id, vendor_id, connection_id, contact_email, method, status, created_at, created_by)
        VALUES ($1, $2, $3, $4, $5, $6, 'unknown', $7, $8)")
        .bind(invite.id).bind(org_id).bind(invite.vendor_id).bind(invite.connection_id)
        .bind(&invite.contact_email).bind(&invite.method).bind(invite.created_at).bind(&invite.created_by)
        .execute(&mut **tx).await?;
    Ok(())
}

pub async fn update_payee_invite(tx: &mut Tx, invite: &PayeeInvite) -> anyhow::Result<()> {
    sqlx::query("UPDATE vendor_payee_invites SET status = $2, mercury_invite_id = $3, proposal_id = $4 WHERE id = $1")
        .bind(invite.id).bind(&invite.status).bind(&invite.mercury_invite_id).bind(invite.proposal_id)
        .execute(&mut **tx).await?;
    Ok(())
}

/// Open invitations Mercury can tell us about, least recently checked first:
/// waiting for the vendor, or completed without a proposal yet. Invitations
/// whose delivery is uncertain need a person, and ones on a replaced
/// connection can't be resolved, so neither is checked.
pub async fn invites_to_check(
    tx: &mut Tx,
    now: DateTime<Utc>,
    every: chrono::Duration,
    limit: i64,
) -> anyhow::Result<Vec<Uuid>> {
    let rows = sqlx::query(
        "SELECT i.id FROM vendor_payee_invites i
         JOIN bank_connections c ON c.id = i.connection_id AND c.retired_at IS NULL
         WHERE i.mercury_invite_id IS NOT NULL AND i.proposal_id IS NULL
           AND i.status IN ('created', 'completed')
           AND (i.checked_at IS NULL OR i.checked_at < $1 - $2)
         ORDER BY i.checked_at NULLS FIRST, i.created_at LIMIT $3",
    )
    .bind(now)
    .bind(every)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(|r| r.get("id")).collect())
}

pub async fn mark_invite_checked(tx: &mut Tx, id: Uuid, now: DateTime<Utc>) -> anyhow::Result<()> {
    sqlx::query("UPDATE vendor_payee_invites SET checked_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Serialize competing invitation commands before any external effect.
pub async fn lock_invite_vendor(tx: &mut Tx, vendor_id: Uuid) -> anyhow::Result<bool> {
    Ok(
        sqlx::query("SELECT id FROM vendors WHERE id = $1 FOR UPDATE")
            .bind(vendor_id)
            .fetch_optional(&mut **tx)
            .await?
            .is_some(),
    )
}

pub mod bill_arrivals;
pub mod purchase_orders;
pub mod ramp;

/// Serialize connection changes and payee decisions, including the empty-bank case.
pub async fn lock_bank(tx: &mut Tx, org: Uuid) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("bank:{org}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

// ── Auto-pay limits ─────────────────────────────────────────────────────────

/// The company's auto-pay limit (None: the default applies) and every
/// vendor's own.
#[derive(Debug, Clone, Default)]
pub struct AutoPayLimits {
    pub company: Option<Cents>,
    pub vendors: std::collections::HashMap<Uuid, Cents>,
}

impl AutoPayLimits {
    pub fn for_vendor(&self, vendor_id: Uuid) -> payvy_domain::workflow::AutoPayLimit {
        payvy_domain::workflow::AutoPayLimit::effective(
            self.company,
            self.vendors.get(&vendor_id).copied(),
        )
    }
}

/// Every limit in the organization. A small table: one row per vendor that
/// has its own limit, plus the company's.
pub async fn auto_pay_limits(tx: &mut Tx) -> anyhow::Result<AutoPayLimits> {
    let rows = sqlx::query("SELECT vendor_id, limit_cents FROM auto_pay_limits")
        .fetch_all(&mut **tx)
        .await?;
    let mut out = AutoPayLimits::default();
    for r in rows {
        match r.get::<Option<Uuid>, _>("vendor_id") {
            Some(v) => {
                out.vendors.insert(v, r.get("limit_cents"));
            }
            None => out.company = Some(r.get("limit_cents")),
        }
    }
    Ok(out)
}

/// The company's and this vendor's limits, for a payment about to be sent.
/// Holds the organization's limit lock (shared) until the transaction ends,
/// so a limit changed at the same moment waits for a payment being decided
/// under the old one instead of racing it, even where no row exists yet.
pub async fn auto_pay_limits_for_payment(
    tx: &mut Tx,
    org_id: Uuid,
    vendor_id: Uuid,
) -> anyhow::Result<(Option<Cents>, Option<Cents>)> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock_shared(hashtextextended('payvy.auto_pay_limits.' || $1::text, 0))",
    )
    .bind(org_id)
    .execute(&mut **tx)
    .await?;
    let rows = sqlx::query(
        "SELECT vendor_id, limit_cents FROM auto_pay_limits
         WHERE vendor_id IS NULL OR vendor_id = $1",
    )
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?;
    let (mut company, mut vendor) = (None, None);
    for r in rows {
        match r.get::<Option<Uuid>, _>("vendor_id") {
            Some(_) => vendor = Some(r.get("limit_cents")),
            None => company = Some(r.get("limit_cents")),
        }
    }
    Ok((company, vendor))
}

/// Sets the company's limit (vendor None) or a vendor's; `limit` None
/// removes a vendor's own limit so it follows the company's.
pub async fn set_auto_pay_limit(
    tx: &mut Tx,
    org_id: Uuid,
    vendor_id: Option<Uuid>,
    limit: Option<Cents>,
    actor: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM auto_pay_limits WHERE vendor_id IS NOT DISTINCT FROM $1")
        .bind(vendor_id)
        .execute(&mut **tx)
        .await?;
    if let Some(cents) = limit {
        sqlx::query(
            "INSERT INTO auto_pay_limits (id, org_id, vendor_id, limit_cents, set_by, set_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(org_id)
        .bind(vendor_id)
        .bind(cents)
        .bind(actor)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Serializes changes to auto-pay limits within one organization, and with
/// automatic payments being sent (`auto_pay_limits_for_payment`).
pub async fn lock_auto_pay_limits(tx: &mut Tx, org_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('payvy.auto_pay_limits.' || $1::text, 0))",
    )
    .bind(org_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Called only inside the transaction that inserts this extraction.
pub async fn set_extraction_analysis(
    tx: &mut Tx,
    id: Uuid,
    analysis: &serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE extractions SET analysis=$2 WHERE id=$1 AND analysis IS NULL")
        .bind(id)
        .bind(analysis)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

// ── Bill batches ────────────────────────────────────────────────────────────

/// "Create N bills" on a file of several invoices: one request, its parts.
#[derive(Debug, Clone, Serialize)]
pub struct BillBatch {
    pub id: Uuid,
    pub document_id: Uuid,
    pub request_id: Uuid,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
}

/// One invoice of a batch: its document, and what became of it.
#[derive(Debug, Clone, Serialize)]
pub struct BillBatchPart {
    pub id: Uuid,
    pub batch_id: Uuid,
    pub position: i32,
    pub document_id: Uuid,
    pub pages: Vec<i32>,
    pub invoice_number: Option<String>,
    pub total_cents: Option<i64>,
    /// reading, created, stacked, needs_you or withdrawn
    pub state: String,
    pub bill_id: Option<Uuid>,
    pub note: Option<String>,
    pub attempts: i32,
    pub updated_at: DateTime<Utc>,
}

macro_rules! batch_cols {
    () => {
        "id, document_id, request_id, requested_by, requested_at"
    };
}
macro_rules! part_cols {
    () => {
        "id, batch_id, position, document_id, pages, invoice_number, total_cents, state, bill_id, note, attempts, updated_at"
    };
}

fn bill_batch(r: &sqlx::postgres::PgRow) -> BillBatch {
    BillBatch {
        id: r.get("id"),
        document_id: r.get("document_id"),
        request_id: r.get("request_id"),
        requested_by: r.get("requested_by"),
        requested_at: r.get("requested_at"),
    }
}

fn bill_batch_part(r: &sqlx::postgres::PgRow) -> BillBatchPart {
    BillBatchPart {
        id: r.get("id"),
        batch_id: r.get("batch_id"),
        position: r.get("position"),
        document_id: r.get("document_id"),
        pages: r.get("pages"),
        invoice_number: r.get("invoice_number"),
        total_cents: r.get("total_cents"),
        state: r.get("state"),
        bill_id: r.get("bill_id"),
        note: r.get("note"),
        attempts: r.get("attempts"),
        updated_at: r.get("updated_at"),
    }
}

/// The batch a request made, if it was made already (a retry or double click).
pub async fn bill_batch_by_request(
    tx: &mut Tx,
    request_id: Uuid,
) -> anyhow::Result<Option<BillBatch>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        batch_cols!(),
        " FROM bill_batches WHERE request_id = $1"
    ))
    .bind(request_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(bill_batch))
}

/// The newest batch made from a document, if any.
pub async fn latest_bill_batch(
    tx: &mut Tx,
    document_id: Uuid,
) -> anyhow::Result<Option<BillBatch>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        batch_cols!(),
        " FROM bill_batches WHERE document_id = $1 ORDER BY requested_at DESC, id LIMIT 1"
    ))
    .bind(document_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(bill_batch))
}

pub async fn insert_bill_batch(
    tx: &mut Tx,
    org_id: Uuid,
    document_id: Uuid,
    request_id: Uuid,
    requested_by: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<BillBatch> {
    let row = sqlx::query(concat!(
        "INSERT INTO bill_batches (id, org_id, document_id, request_id, requested_by, requested_at)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING ",
        batch_cols!()
    ))
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(document_id)
    .bind(request_id)
    .bind(requested_by)
    .bind(at)
    .fetch_one(&mut **tx)
    .await?;
    Ok(bill_batch(&row))
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_bill_batch_part(
    tx: &mut Tx,
    org_id: Uuid,
    batch_id: Uuid,
    position: i32,
    document_id: Uuid,
    pages: &[i32],
    invoice_number: Option<&str>,
    total_cents: Option<i64>,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO bill_batch_parts (id, org_id, batch_id, position, document_id, pages, invoice_number, total_cents, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(batch_id)
    .bind(position)
    .bind(document_id)
    .bind(pages)
    .bind(invoice_number)
    .bind(total_cents)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A batch's parts, in the order they were laid out.
pub async fn bill_batch_parts(tx: &mut Tx, batch_id: Uuid) -> anyhow::Result<Vec<BillBatchPart>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        part_cols!(),
        " FROM bill_batch_parts WHERE batch_id = $1 ORDER BY position"
    ))
    .bind(batch_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(bill_batch_part).collect())
}

/// Up to `limit` parts still being read, claimed until `now + lease` so no
/// other worker takes them meanwhile, each with who asked for the batch.
pub async fn claim_bill_batch_parts(
    tx: &mut Tx,
    now: DateTime<Utc>,
    lease: chrono::Duration,
    limit: i64,
) -> anyhow::Result<Vec<(BillBatchPart, String)>> {
    let rows = sqlx::query(
        "UPDATE bill_batch_parts p SET claimed_until = $1 + $2
         FROM bill_batches b
         WHERE b.id = p.batch_id AND p.id IN (
             SELECT id FROM bill_batch_parts
             WHERE state = 'reading' AND (claimed_until IS NULL OR claimed_until < $1)
             ORDER BY updated_at, position LIMIT $3 FOR UPDATE SKIP LOCKED)
         RETURNING p.id, p.batch_id, p.position, p.document_id, p.pages, p.invoice_number,
                   p.total_cents, p.state, p.bill_id, p.note, p.attempts, p.updated_at, b.requested_by",
    )
    .bind(now)
    .bind(lease)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    let mut out: Vec<(BillBatchPart, String)> = rows
        .iter()
        .map(|r| (bill_batch_part(r), r.get("requested_by")))
        .collect();
    out.sort_by_key(|(p, _)| (p.batch_id, p.position));
    Ok(out)
}

/// What became of a part; it is no longer claimed.
pub async fn finish_bill_batch_part(
    tx: &mut Tx,
    id: Uuid,
    state: &str,
    bill_id: Option<Uuid>,
    note: Option<&str>,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE bill_batch_parts SET state = $2, bill_id = $3, note = $4, updated_at = $5, claimed_until = NULL
         WHERE id = $1",
    )
    .bind(id)
    .bind(state)
    .bind(bill_id)
    .bind(note)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Not decided this time: try again on a later pass, after a short wait so a
/// part that keeps conflicting can't spin the worker. `failed` counts it as a failed try.
pub async fn release_bill_batch_part(
    tx: &mut Tx,
    id: Uuid,
    failed: bool,
    at: DateTime<Utc>,
) -> anyhow::Result<i32> {
    let row = sqlx::query(
        "UPDATE bill_batch_parts SET claimed_until = $3 + interval '30 seconds', updated_at = $3,
                attempts = attempts + CASE WHEN $2 THEN 1 ELSE 0 END
         WHERE id = $1 RETURNING attempts",
    )
    .bind(id)
    .bind(failed)
    .bind(at)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.get("attempts"))
}
