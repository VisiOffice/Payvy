//! Purchase orders, revisions, lines and the allocation ledger.
//! Every query runs inside the caller's org-scoped transaction (RLS).

use chrono::{DateTime, NaiveDate, Utc};
use payvy_domain::Cents;
use payvy_domain::purchase_orders::Amounts;
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::Tx;

#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    pub id: Uuid,
    pub revision: i32,
    pub authorized_cents: Cents,
    pub overage_cents_cap: Option<Cents>,
    pub overage_percent_bp: Option<i32>,
    pub allowance_cents: Cents,
    pub reason: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PurchaseOrder {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub vendor_name: String,
    pub po_number: String,
    pub status: String,
    pub currency: String,
    pub owner: Option<String>,
    pub activated_by: Option<String>,
    pub external_source: Option<String>,
    pub external_id: Option<String>,
    pub service_address: Option<String>,
    pub period_start: Option<NaiveDate>,
    pub period_end: Option<NaiveDate>,
    pub vendor_reference: Option<String>,
    pub metadata: Value,
    pub final_period: bool,
    /// Last `po.upsert` body. A person editing the PO clears it.
    #[serde(skip)]
    pub body_hash: Option<String>,
    /// A person edited this PO. A later sync that would change it is refused.
    #[serde(skip)]
    pub person_hold: bool,
    pub lifecycle_reason: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip)]
    pub revision: Option<Revision>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoLine {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub position: i32,
    pub description: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub unit_price_cents: Option<Cents>,
    pub amount_cents: Cents,
    pub account: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Allocation {
    pub id: Uuid,
    pub po_id: Uuid,
    pub revision_id: Uuid,
    pub bill_id: Option<Uuid>,
    pub amount_cents: Cents,
    pub state: String,
    pub kind: String,
    pub release_reason: Option<String>,
}

fn revision(r: &PgRow) -> Revision {
    Revision {
        id: r.get("rid"),
        revision: r.get("revision"),
        authorized_cents: r.get("authorized_cents"),
        overage_cents_cap: r.get("overage_cents_cap"),
        overage_percent_bp: r.get("overage_percent_bp"),
        allowance_cents: r.get("allowance_cents"),
        reason: r.get("rreason"),
        created_by: r.get("rcreated_by"),
        created_at: r.get("rcreated_at"),
    }
}

fn po(r: &PgRow) -> PurchaseOrder {
    let rev = r.get::<Option<Uuid>, _>("rid").map(|_| revision(r));
    PurchaseOrder {
        id: r.get("id"),
        vendor_id: r.get("vendor_id"),
        vendor_name: r.get("vendor_name"),
        po_number: r.get("po_number"),
        status: r.get("status"),
        currency: r.get("currency"),
        owner: r.get("owner"),
        activated_by: r.get("activated_by"),
        external_source: r.get("external_source"),
        external_id: r.get("external_id"),
        service_address: r.get("service_address"),
        period_start: r.get("period_start"),
        period_end: r.get("period_end"),
        vendor_reference: r.get("vendor_reference"),
        metadata: r.get("metadata"),
        final_period: r.get("final_period"),
        body_hash: r.get("body_hash"),
        person_hold: r.get("person_hold"),
        lifecycle_reason: r.get("lifecycle_reason"),
        created_by: r.get("created_by"),
        created_at: r.get("created_at"),
        revision: rev,
    }
}

macro_rules! po_query {
    ($tail:literal) => { concat!("SELECT p.id, p.vendor_id, v.name AS vendor_name, p.po_number, p.status, p.currency, p.owner, p.activated_by, p.external_source, p.external_id, p.service_address, p.period_start, p.period_end, p.vendor_reference, p.metadata, p.final_period, p.body_hash, p.person_hold, p.lifecycle_reason, p.created_by, p.created_at, r.id AS rid, r.revision, r.authorized_cents, r.overage_cents_cap, r.overage_percent_bp, r.allowance_cents, r.reason AS rreason, r.created_by AS rcreated_by, r.created_at AS rcreated_at", " FROM purchase_orders p JOIN vendors v ON v.id = p.vendor_id LEFT JOIN LATERAL (SELECT * FROM po_revisions WHERE po_id = p.id ORDER BY revision DESC LIMIT 1) r ON true", $tail) };
}

pub async fn get_po(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<PurchaseOrder>> {
    let row = sqlx::query(po_query!(" WHERE p.id = $1"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.as_ref().map(po))
}

pub async fn list_pos(tx: &mut Tx, vendor_id: Option<Uuid>) -> anyhow::Result<Vec<PurchaseOrder>> {
    let rows = sqlx::query(po_query!(
        " WHERE ($1::uuid IS NULL OR p.vendor_id = $1) ORDER BY p.created_at"
    ))
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(po).collect())
}

/// Lock PO rows in a consistent (sorted) order; multi-PO splits are atomic.
pub async fn lock_pos(tx: &mut Tx, ids: &[Uuid]) -> anyhow::Result<Vec<PurchaseOrder>> {
    let mut sorted = ids.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut out = Vec::new();
    for id in sorted {
        let locked = sqlx::query("SELECT id FROM purchase_orders WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
        if locked.is_none() {
            return Ok(out);
        }
        if let Some(p) = get_po(tx, id).await? {
            out.push(p);
        }
    }
    Ok(out)
}

pub struct NewPo<'a> {
    pub vendor_id: Uuid,
    pub po_number: &'a str,
    pub owner: Option<&'a str>,
    pub external_source: Option<&'a str>,
    pub external_id: Option<&'a str>,
    pub service_address: Option<&'a str>,
    pub period_start: Option<NaiveDate>,
    pub period_end: Option<NaiveDate>,
    pub created_by: &'a str,
}

pub async fn insert_po(
    tx: &mut Tx,
    org: Uuid,
    n: &NewPo<'_>,
    now: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO purchase_orders (id, org_id, vendor_id, po_number, owner, external_source, external_id, service_address, period_start, period_end, created_by, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$12)")
        .bind(id).bind(org).bind(n.vendor_id).bind(n.po_number).bind(n.owner)
        .bind(n.external_source).bind(n.external_id).bind(n.service_address)
        .bind(n.period_start).bind(n.period_end).bind(n.created_by).bind(now)
        .execute(&mut **tx).await?;
    Ok(id)
}

#[derive(Clone)]
pub struct NewLine {
    pub description: String,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub unit_price_cents: Option<Cents>,
    pub amount_cents: Cents,
    pub account: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_revision(
    tx: &mut Tx,
    org: Uuid,
    po_id: Uuid,
    rev_no: i32,
    authorized: Cents,
    dollar_cap: Option<Cents>,
    percent_bp: Option<i32>,
    allowance: Cents,
    reason: Option<&str>,
    lines: &[NewLine],
    by: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO po_revisions (id, org_id, po_id, revision, authorized_cents, overage_cents_cap, overage_percent_bp, allowance_cents, reason, created_by, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(id).bind(org).bind(po_id).bind(rev_no).bind(authorized).bind(dollar_cap)
        .bind(percent_bp).bind(allowance).bind(reason).bind(by).bind(now)
        .execute(&mut **tx).await?;
    for (i, l) in lines.iter().enumerate() {
        sqlx::query("INSERT INTO po_lines (id, org_id, po_id, revision_id, position, description, quantity, unit, unit_price_cents, amount_cents, account) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(Uuid::new_v4()).bind(org).bind(po_id).bind(id).bind(i as i32 + 1)
            .bind(&l.description).bind(l.quantity).bind(&l.unit).bind(l.unit_price_cents)
            .bind(l.amount_cents).bind(&l.account).execute(&mut **tx).await?;
    }
    sqlx::query("UPDATE purchase_orders SET updated_at = $2 WHERE id = $1")
        .bind(po_id)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    Ok(id)
}

pub async fn lines_for(tx: &mut Tx, revision_id: Uuid) -> anyhow::Result<Vec<PoLine>> {
    let rows = sqlx::query("SELECT id, revision_id, position, description, quantity, unit, unit_price_cents, amount_cents, account FROM po_lines WHERE revision_id = $1 ORDER BY position")
        .bind(revision_id).fetch_all(&mut **tx).await?;
    Ok(rows
        .iter()
        .map(|r| PoLine {
            id: r.get("id"),
            revision_id: r.get("revision_id"),
            position: r.get("position"),
            description: r.get("description"),
            quantity: r.get("quantity"),
            unit: r.get("unit"),
            unit_price_cents: r.get("unit_price_cents"),
            amount_cents: r.get("amount_cents"),
            account: r.get("account"),
        })
        .collect())
}

pub async fn set_status(
    tx: &mut Tx,
    id: Uuid,
    status: &str,
    activated_by: Option<&str>,
    reason: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE purchase_orders SET status = $2, activated_by = coalesce($3, activated_by), lifecycle_reason = $4, updated_at = $5 WHERE id = $1")
        .bind(id).bind(status).bind(activated_by).bind(reason).bind(now).execute(&mut **tx).await?;
    Ok(())
}

pub async fn amounts(tx: &mut Tx, p: &PurchaseOrder) -> anyhow::Result<Amounts> {
    let rev = p.revision.as_ref();
    let row = sqlx::query("SELECT coalesce(sum(amount_cents) FILTER (WHERE state = 'reserved'),0)::bigint AS reserved, coalesce(sum(amount_cents) FILTER (WHERE state = 'consumed'),0)::bigint AS consumed FROM po_allocations WHERE po_id = $1")
        .bind(p.id).fetch_one(&mut **tx).await?;
    Ok(Amounts {
        authorized_cents: rev.map_or(0, |r| r.authorized_cents),
        reserved_cents: row.get("reserved"),
        consumed_cents: row.get("consumed"),
        allowance_cents: rev.map_or(0, |r| r.allowance_cents),
    })
}

fn alloc(r: &PgRow) -> Allocation {
    Allocation {
        id: r.get("id"),
        po_id: r.get("po_id"),
        revision_id: r.get("revision_id"),
        bill_id: r.get("bill_id"),
        amount_cents: r.get("amount_cents"),
        state: r.get("state"),
        kind: r.get("kind"),
        release_reason: r.get("release_reason"),
    }
}

pub async fn allocations_for(tx: &mut Tx, po_id: Uuid) -> anyhow::Result<Vec<Allocation>> {
    let rows = sqlx::query("SELECT id, po_id, revision_id, bill_id, amount_cents, state, kind, release_reason FROM po_allocations WHERE po_id = $1 ORDER BY created_at")
        .bind(po_id).fetch_all(&mut **tx).await?;
    Ok(rows.iter().map(alloc).collect())
}

/// Lock a bill's live allocation rows for the rest of the transaction,
/// in a stable order. Held across the payment gate's check, a concurrent
/// allocation_reverse (whose UPDATE needs these rows) waits, and its
/// change is then visible to the gate's re-read — never the other way.
pub async fn lock_allocations_for_bill(
    tx: &mut Tx,
    bill_id: Uuid,
) -> anyhow::Result<Vec<Allocation>> {
    let rows = sqlx::query("SELECT id, po_id, revision_id, bill_id, amount_cents, state, kind, release_reason FROM po_allocations WHERE bill_id = $1 AND state <> 'released' ORDER BY seq FOR UPDATE")
        .bind(bill_id).fetch_all(&mut **tx).await?;
    Ok(rows.iter().map(alloc).collect())
}

pub async fn allocations_for_bill(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Vec<Allocation>> {
    let rows = sqlx::query("SELECT id, po_id, revision_id, bill_id, amount_cents, state, kind, release_reason FROM po_allocations WHERE bill_id = $1 AND state <> 'released' ORDER BY created_at")
        .bind(bill_id).fetch_all(&mut **tx).await?;
    Ok(rows.iter().map(alloc).collect())
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_allocation(
    tx: &mut Tx,
    org: Uuid,
    po_id: Uuid,
    revision_id: Uuid,
    bill_id: Option<Uuid>,
    amount: Cents,
    state: &str,
    kind: &str,
    actor: &str,
    ext_source: Option<&str>,
    ext_id: Option<&str>,
    source_alloc: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO po_allocations (id, org_id, po_id, revision_id, bill_id, amount_cents, state, kind, source_actor, source_external_source, source_external_id, source_allocation_id, created_at, accepted_at, accepted_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13, CASE WHEN $7 = 'consumed' THEN $13 END, CASE WHEN $7 = 'consumed' THEN $9 END)")
        .bind(id).bind(org).bind(po_id).bind(revision_id).bind(bill_id).bind(amount)
        .bind(state).bind(kind).bind(actor).bind(ext_source).bind(ext_id).bind(source_alloc).bind(now)
        .execute(&mut **tx).await?;
    Ok(id)
}

pub async fn get_allocation(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Allocation>> {
    let row = sqlx::query("SELECT id, po_id, revision_id, bill_id, amount_cents, state, kind, release_reason FROM po_allocations WHERE id = $1")
        .bind(id).fetch_optional(&mut **tx).await?;
    Ok(row.as_ref().map(alloc))
}

pub async fn set_allocation_state(
    tx: &mut Tx,
    id: Uuid,
    state: &str,
    by: &str,
    reason: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE po_allocations SET state = $2, accepted_at = CASE WHEN $2 = 'consumed' THEN $3 END, accepted_by = CASE WHEN $2 = 'consumed' THEN $4 END, released_at = CASE WHEN $2 = 'released' THEN $3 END, released_by = CASE WHEN $2 = 'released' THEN $4 END, release_reason = CASE WHEN $2 = 'released' THEN $5 END WHERE id = $1")
        .bind(id).bind(state).bind(now).bind(by).bind(reason).execute(&mut **tx).await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct VendorSettings {
    pub require_po: bool,
    pub require_funding: bool,
    pub overage_cents_cap: Option<Cents>,
    pub overage_percent_bp: Option<i32>,
}

pub async fn vendor_settings(tx: &mut Tx, vendor_id: Uuid) -> anyhow::Result<VendorSettings> {
    let row = sqlx::query("SELECT require_po, require_funding, overage_cents_cap, overage_percent_bp FROM vendor_po_settings WHERE vendor_id = $1")
        .bind(vendor_id).fetch_optional(&mut **tx).await?;
    Ok(match row {
        Some(r) => VendorSettings {
            require_po: r.get("require_po"),
            require_funding: r.get("require_funding"),
            overage_cents_cap: r.get("overage_cents_cap"),
            overage_percent_bp: r.get("overage_percent_bp"),
        },
        None => VendorSettings {
            require_po: false,
            require_funding: false,
            overage_cents_cap: None,
            overage_percent_bp: None,
        },
    })
}

pub async fn set_vendor_settings(
    tx: &mut Tx,
    org: Uuid,
    vendor_id: Uuid,
    s: &VendorSettings,
    by: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO vendor_po_settings (id, org_id, vendor_id, require_po, require_funding, overage_cents_cap, overage_percent_bp, updated_by, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT (org_id, vendor_id) DO UPDATE SET require_po = $4, require_funding = $5, overage_cents_cap = $6, overage_percent_bp = $7, updated_by = $8, updated_at = $9")
        .bind(Uuid::new_v4()).bind(org).bind(vendor_id).bind(s.require_po).bind(s.require_funding).bind(s.overage_cents_cap).bind(s.overage_percent_bp).bind(by).bind(now)
        .execute(&mut **tx).await?;
    Ok(())
}

pub async fn po_by_number(tx: &mut Tx, po_number: &str) -> anyhow::Result<Option<PurchaseOrder>> {
    let row = sqlx::query("SELECT id FROM purchase_orders WHERE po_number = $1")
        .bind(po_number)
        .fetch_optional(&mut **tx)
        .await?;
    match row {
        Some(r) => get_po(tx, r.get("id")).await,
        None => Ok(None),
    }
}

pub async fn po_by_external(
    tx: &mut Tx,
    source: &str,
    ext_id: &str,
) -> anyhow::Result<Option<PurchaseOrder>> {
    let row = sqlx::query(
        "SELECT id FROM purchase_orders WHERE external_source = $1 AND external_id = $2",
    )
    .bind(source)
    .bind(ext_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(r) => get_po(tx, r.get("id")).await,
        None => Ok(None),
    }
}

pub async fn update_po_details(
    tx: &mut Tx,
    id: Uuid,
    owner: Option<&str>,
    service_address: Option<&str>,
    period_start: Option<NaiveDate>,
    period_end: Option<NaiveDate>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE purchase_orders SET owner = $2, service_address = $3, period_start = $4, period_end = $5, updated_at = $6 WHERE id = $1")
        .bind(id).bind(owner).bind(service_address).bind(period_start).bind(period_end).bind(now)
        .execute(&mut **tx).await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Override {
    pub id: Uuid,
    pub bill_id: Uuid,
    pub exception_kind: String,
    pub authorized_excess_cents: Cents,
    pub bill_amount_cents: Cents,
    pub fingerprint: String,
    pub reason: String,
    pub created_by: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_override(
    tx: &mut Tx,
    org: Uuid,
    bill_id: Uuid,
    kind: &str,
    excess: Cents,
    bill_amount: Cents,
    fingerprint: &str,
    reason: &str,
    by: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO po_overrides (id, org_id, bill_id, exception_kind, authorized_excess_cents, bill_amount_cents, fingerprint, reason, created_by, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(id).bind(org).bind(bill_id).bind(kind).bind(excess).bind(bill_amount).bind(fingerprint).bind(reason).bind(by).bind(now)
        .execute(&mut **tx).await?;
    Ok(id)
}

/// Every override ever recorded for this bill, newest first.
pub async fn overrides_for_bill(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Vec<Override>> {
    let rows = sqlx::query("SELECT id, bill_id, exception_kind, authorized_excess_cents, bill_amount_cents, fingerprint, reason, created_by FROM po_overrides WHERE bill_id = $1 ORDER BY created_at DESC")
        .bind(bill_id).fetch_all(&mut **tx).await?;
    Ok(rows
        .iter()
        .map(|r| Override {
            id: r.get("id"),
            bill_id: r.get("bill_id"),
            exception_kind: r.get("exception_kind"),
            authorized_excess_cents: r.get("authorized_excess_cents"),
            bill_amount_cents: r.get("bill_amount_cents"),
            fingerprint: r.get("fingerprint"),
            reason: r.get("reason"),
            created_by: r.get("created_by"),
        })
        .collect())
}

pub async fn latest_override(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Option<Override>> {
    let row = sqlx::query("SELECT id, bill_id, exception_kind, authorized_excess_cents, bill_amount_cents, fingerprint, reason, created_by FROM po_overrides WHERE bill_id = $1 ORDER BY created_at DESC LIMIT 1")
        .bind(bill_id).fetch_optional(&mut **tx).await?;
    Ok(row.map(|r| Override {
        id: r.get("id"),
        bill_id: r.get("bill_id"),
        exception_kind: r.get("exception_kind"),
        authorized_excess_cents: r.get("authorized_excess_cents"),
        bill_amount_cents: r.get("bill_amount_cents"),
        fingerprint: r.get("fingerprint"),
        reason: r.get("reason"),
        created_by: r.get("created_by"),
    }))
}

pub async fn bill_has_signal(tx: &mut Tx, bill_id: Uuid, key: &str) -> anyhow::Result<bool> {
    let row = sqlx::query("SELECT id FROM signals WHERE bill_id = $1 AND key = $2")
        .bind(bill_id)
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.is_some())
}

/// Release every live allocation for a bill (bill rejected/voided). Returns
/// the allocations released so the caller can emit one event each.
pub async fn reverse_allocations_for_bill(
    tx: &mut Tx,
    bill_id: Uuid,
    by: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Allocation>> {
    let allocs = allocations_for_bill(tx, bill_id).await?;
    for a in &allocs {
        set_allocation_state(tx, a.id, "released", by, Some(reason), now).await?;
    }
    Ok(allocs)
}

/// The company-wide purchase-orders switch. No row means off.
#[derive(Debug, Clone, Serialize)]
pub struct CompanySettings {
    pub enabled: bool,
    pub updated_at: Option<DateTime<Utc>>,
    pub updated_by: Option<String>,
}

pub async fn company_settings(tx: &mut Tx) -> anyhow::Result<CompanySettings> {
    let row = sqlx::query("SELECT enabled, updated_at, updated_by FROM po_company_settings")
        .fetch_optional(&mut **tx)
        .await?;
    Ok(match row {
        Some(r) => CompanySettings {
            enabled: r.get("enabled"),
            updated_at: Some(r.get("updated_at")),
            updated_by: Some(r.get("updated_by")),
        },
        None => CompanySettings {
            enabled: false,
            updated_at: None,
            updated_by: None,
        },
    })
}

pub async fn set_company_settings(
    tx: &mut Tx,
    org: Uuid,
    enabled: bool,
    by: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO po_company_settings (org_id, enabled, updated_at, updated_by) VALUES ($1, $2, $3, $4) ON CONFLICT (org_id) DO UPDATE SET enabled = $2, updated_at = $3, updated_by = $4")
        .bind(org).bind(enabled).bind(now).bind(by)
        .execute(&mut **tx).await?;
    Ok(())
}

/// Serializes changes to the switch, so a no-op is judged against the
/// value that is really there.
pub async fn lock_company_settings(tx: &mut Tx, org: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('payvy.po_settings.' || $1::text, 0))",
    )
    .bind(org)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ── The enumerated policy registry ───────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct PolicyVersion {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub version: i32,
    pub status: String,
    /// flag_only | require_full_cover (payvy-domain::purchase_orders::OverPeriod)
    pub over_period: String,
    /// flag_only | enforce (payvy-domain::purchase_orders::QuantityMatching)
    pub quantity_matching: String,
    pub reason: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

fn policy_version(r: &PgRow) -> PolicyVersion {
    PolicyVersion {
        id: r.get("id"),
        vendor_id: r.get("vendor_id"),
        version: r.get("version"),
        status: r.get("status"),
        over_period: r.get("over_period"),
        quantity_matching: r.get("quantity_matching"),
        reason: r.get("reason"),
        created_by: r.get("created_by"),
        created_at: r.get("created_at"),
    }
}

macro_rules! policy_cols {
    () => {
        "id, vendor_id, version, status, over_period, quantity_matching, reason, created_by, created_at"
    };
}

/// The vendor's policy version in force now, if a person has ever set one.
pub async fn current_policy(tx: &mut Tx, vendor_id: Uuid) -> anyhow::Result<Option<PolicyVersion>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        policy_cols!(),
        " FROM vendor_po_policies WHERE vendor_id = $1 AND status = 'active'"
    ))
    .bind(vendor_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(policy_version))
}

pub async fn policy_by_id(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<PolicyVersion>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        policy_cols!(),
        " FROM vendor_po_policies WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(policy_version))
}

/// A person's change: the active version is superseded and a new one takes
/// its place. Old versions are never edited, so a bill's pin keeps meaning
/// what it meant when the bill was created.
#[allow(clippy::too_many_arguments)]
pub async fn set_policy(
    tx: &mut Tx,
    org: Uuid,
    vendor_id: Uuid,
    over_period: &str,
    quantity_matching: &str,
    reason: Option<&str>,
    by: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<PolicyVersion> {
    // Serialize policy changes per vendor: two first-ever sets would
    // otherwise both compute version 1 and one would fail on the unique
    // constraint instead of superseding (or refusing) cleanly.
    sqlx::query("SELECT id FROM vendors WHERE id = $1 FOR UPDATE")
        .bind(vendor_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "UPDATE vendor_po_policies SET status = 'superseded' WHERE vendor_id = $1 AND status = 'active'",
    )
    .bind(vendor_id)
    .execute(&mut **tx)
    .await?;
    let version: i32 = sqlx::query(
        "SELECT COALESCE(MAX(version), 0) + 1 AS v FROM vendor_po_policies WHERE vendor_id = $1",
    )
    .bind(vendor_id)
    .fetch_one(&mut **tx)
    .await?
    .get("v");
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO vendor_po_policies (id, org_id, vendor_id, version, status, over_period, quantity_matching, reason, created_by, created_at) VALUES ($1,$2,$3,$4,'active',$5,$6,$7,$8,$9)",
    )
    .bind(id).bind(org).bind(vendor_id).bind(version).bind(over_period).bind(quantity_matching).bind(reason).bind(by).bind(now)
    .execute(&mut **tx)
    .await?;
    policy_by_id(tx, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the policy version just written is missing"))
}

/// One bill's stake in a PO: when it first allocated, how much of the
/// bill this PO carries (the split share), and its lines.
#[derive(Debug, Clone)]
pub struct AllocatedBill {
    pub bill_id: Uuid,
    pub first_seq: i64,
    pub allocated_cents: Cents,
    /// The bill's live allocations across every PO: the denominator of
    /// its split share, so an unallocated remainder never shrinks it.
    pub total_allocated_cents: Cents,
    pub bill_amount_cents: Cents,
    pub lines: Vec<payvy_domain::bills::Line>,
}

/// Every bill with a live (not released) allocation to this PO, once each
/// (a bill may hold several allocations), in first-allocation order.
pub async fn allocated_bill_lines(tx: &mut Tx, po_id: Uuid) -> anyhow::Result<Vec<AllocatedBill>> {
    let rows = sqlx::query(
        "WITH per_bill AS (\
             SELECT a.bill_id, min(a.seq) AS first_seq, sum(a.amount_cents)::bigint AS allocated, \
                    (SELECT sum(x.amount_cents)::bigint FROM po_allocations x \
                      WHERE x.bill_id = a.bill_id AND x.state <> 'released') AS total_allocated \
             FROM po_allocations a \
             WHERE a.po_id = $1 AND a.state <> 'released' AND a.bill_id IS NOT NULL \
             GROUP BY a.bill_id) \
         SELECT pb.bill_id AS bill_id, pb.first_seq AS first_seq, pb.allocated AS allocated, pb.total_allocated AS total_allocated, \
                b.amount_cents AS bill_amount, bl.description, bl.amount_cents, bl.account, \
                bl.quantity_micro, bl.unit, bl.unit_price_cents \
         FROM per_bill pb \
         JOIN bills b ON b.id = pb.bill_id \
         JOIN bill_lines bl ON bl.bill_id = pb.bill_id \
         ORDER BY pb.first_seq, pb.bill_id, bl.position",
    )
    .bind(po_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut out: Vec<AllocatedBill> = Vec::new();
    for r in &rows {
        let bill_id: Uuid = r.get("bill_id");
        let micro: Option<i64> = r.get("quantity_micro");
        let line = payvy_domain::bills::Line {
            description: r.get("description"),
            amount_cents: r.get("amount_cents"),
            account: r.get("account"),
            quantity: micro.map(payvy_domain::invoice_evidence::format_micro),
            unit: r.get("unit"),
            unit_price_cents: r.get("unit_price_cents"),
        };
        match out.iter_mut().find(|b| b.bill_id == bill_id) {
            Some(b) => b.lines.push(line),
            None => out.push(AllocatedBill {
                bill_id,
                first_seq: r.get("first_seq"),
                allocated_cents: r.get("allocated"),
                total_allocated_cents: r.get("total_allocated"),
                bill_amount_cents: r.get("bill_amount"),
                lines: vec![line],
            }),
        }
    }
    Ok(out)
}

/// Lock the PO with this external id, if it exists.
pub async fn lock_po_by_external(
    tx: &mut Tx,
    source: &str,
    ext_id: &str,
) -> anyhow::Result<Option<PurchaseOrder>> {
    let row = sqlx::query(
        "SELECT id FROM purchase_orders WHERE external_source = $1 AND external_id = $2 FOR UPDATE",
    )
    .bind(source)
    .bind(ext_id)
    .fetch_optional(&mut **tx)
    .await?;
    match row {
        Some(r) => get_po(tx, r.get("id")).await,
        None => Ok(None),
    }
}

pub struct FeedFields<'a> {
    pub owner: Option<&'a str>,
    /// When false, an update leaves the stored owner alone. An omitted
    /// owner and an owner sent as empty both arrive as `None`, so the
    /// caller says which one this request was.
    pub write_owner: bool,
    pub service_address: Option<&'a str>,
    pub period_start: Option<NaiveDate>,
    pub period_end: Option<NaiveDate>,
    pub vendor_reference: Option<&'a str>,
    pub metadata: &'a Value,
    pub final_period: bool,
    pub body_hash: Option<&'a str>,
}

pub struct NewFeedPo<'a> {
    pub vendor_id: Uuid,
    pub po_number: &'a str,
    pub external_source: &'a str,
    pub external_id: &'a str,
    pub created_by: &'a str,
    pub feed: FeedFields<'a>,
}

/// Insert a PO. `None` means another transaction already holds this external id.
pub async fn insert_feed_po(
    tx: &mut Tx,
    org: Uuid,
    po: &NewFeedPo<'_>,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<Uuid>> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        "INSERT INTO purchase_orders (id, org_id, vendor_id, po_number, owner, external_source, external_id, service_address, period_start, period_end, vendor_reference, metadata, final_period, body_hash, created_by, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$16) \
         ON CONFLICT (org_id, external_source, external_id) WHERE external_source IS NOT NULL DO NOTHING \
         RETURNING id",
    )
    .bind(id)
    .bind(org)
    .bind(po.vendor_id)
    .bind(po.po_number)
    .bind(po.feed.owner)
    .bind(po.external_source)
    .bind(po.external_id)
    .bind(po.feed.service_address)
    .bind(po.feed.period_start)
    .bind(po.feed.period_end)
    .bind(po.feed.vendor_reference)
    .bind(po.feed.metadata)
    .bind(po.feed.final_period)
    .bind(po.feed.body_hash)
    .bind(po.created_by)
    .bind(now)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| r.get("id")))
}

pub async fn update_feed(
    tx: &mut Tx,
    id: Uuid,
    feed: &FeedFields<'_>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE purchase_orders SET owner = CASE WHEN $2 THEN $3 ELSE owner END, service_address = $4, period_start = $5, period_end = $6, vendor_reference = $7, metadata = $8, final_period = $9, body_hash = $10, updated_at = $11 WHERE id = $1",
    )
    .bind(id)
    .bind(feed.write_owner)
    .bind(feed.owner)
    .bind(feed.service_address)
    .bind(feed.period_start)
    .bind(feed.period_end)
    .bind(feed.vendor_reference)
    .bind(feed.metadata)
    .bind(feed.final_period)
    .bind(feed.body_hash)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A person's edit is no longer the last synced body, and a later sync cannot change it.
pub async fn hold_for_person(tx: &mut Tx, id: Uuid, now: DateTime<Utc>) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE purchase_orders SET body_hash = NULL, person_hold = true, updated_at = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A person lifted the hold. The stored body hash stays unset until the next sync.
pub async fn clear_person_hold(tx: &mut Tx, id: Uuid, now: DateTime<Utc>) -> anyhow::Result<()> {
    sqlx::query("UPDATE purchase_orders SET person_hold = false, updated_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SourceAuth {
    pub id: Uuid,
    pub source: String,
    pub key_id: Uuid,
    pub max_cents: Cents,
    pub owner: String,
    pub authorized_by: String,
    pub revoked_at: Option<DateTime<Utc>>,
}

fn source_auth_row(r: &PgRow) -> SourceAuth {
    SourceAuth {
        id: r.get("id"),
        source: r.get("source"),
        key_id: r.get("key_id"),
        max_cents: r.get("max_cents"),
        owner: r.get("owner"),
        authorized_by: r.get("authorized_by"),
        revoked_at: r.get("revoked_at"),
    }
}

/// Active authorizations for this key and vendor. The caller picks the
/// one whose stored source matches. A revoked key matches nothing.
pub async fn active_auths_for_key(
    tx: &mut Tx,
    key_id: Uuid,
    vendor_id: Uuid,
) -> anyhow::Result<Vec<SourceAuth>> {
    let rows = sqlx::query(
        "SELECT a.id, a.source, a.key_id, a.max_cents, a.owner, a.authorized_by, a.revoked_at \
         FROM po_source_authorizations a \
         JOIN po_source_authorization_vendors v ON v.authorization_id = a.id \
         JOIN api_keys k ON k.id = a.key_id AND k.revoked_at IS NULL \
         WHERE a.revoked_at IS NULL AND a.key_id = $1 AND v.vendor_id = $2",
    )
    .bind(key_id)
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(source_auth_row).collect())
}

/// This key may close or cancel a PO of this source and vendor.
pub async fn key_can_end(
    tx: &mut Tx,
    key_id: Uuid,
    source: &str,
    vendor_id: Uuid,
) -> anyhow::Result<bool> {
    let row = sqlx::query(
        "SELECT 1 AS ok FROM po_source_authorizations a \
         JOIN po_source_authorization_vendors v ON v.authorization_id = a.id \
         JOIN api_keys k ON k.id = a.key_id AND k.revoked_at IS NULL \
         WHERE a.revoked_at IS NULL AND a.key_id = $1 AND a.source = $2 AND v.vendor_id = $3 \
         LIMIT 1",
    )
    .bind(key_id)
    .bind(source)
    .bind(vendor_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.is_some())
}

/// Some unrevoked authorization already covers this source.
pub async fn source_has_active_auth(tx: &mut Tx, source: &str) -> anyhow::Result<bool> {
    let row = sqlx::query(
        "SELECT 1 AS ok FROM po_source_authorizations \
         WHERE revoked_at IS NULL AND source = $1 LIMIT 1",
    )
    .bind(source)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.is_some())
}

pub async fn source_auth(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<SourceAuth>> {
    let row = sqlx::query(
        "SELECT id, source, key_id, max_cents, owner, authorized_by, revoked_at FROM po_source_authorizations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(source_auth_row))
}

pub async fn lock_vendors(tx: &mut Tx, vendor_ids: &[Uuid]) -> anyhow::Result<usize> {
    let rows = sqlx::query("SELECT id FROM vendors WHERE id = ANY($1) ORDER BY id FOR UPDATE")
        .bind(vendor_ids)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows.len())
}

/// Vendors in `vendor_ids` that already have an active authorization for `source`.
pub async fn vendors_already_authorized(
    tx: &mut Tx,
    source: &str,
    vendor_ids: &[Uuid],
) -> anyhow::Result<Vec<Uuid>> {
    let rows = sqlx::query(
        "SELECT v.vendor_id FROM po_source_authorizations a \
         JOIN po_source_authorization_vendors v ON v.authorization_id = a.id \
         WHERE a.revoked_at IS NULL AND a.source = $1 AND v.vendor_id = ANY($2) \
         FOR UPDATE OF a",
    )
    .bind(source)
    .bind(vendor_ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(|r| r.get("vendor_id")).collect())
}

pub struct NewSourceAuth<'a> {
    pub source: &'a str,
    pub key_id: Uuid,
    pub max_cents: Cents,
    pub owner: &'a str,
    pub vendor_ids: &'a [Uuid],
    pub by: &'a str,
}

pub async fn insert_source_auth(
    tx: &mut Tx,
    org: Uuid,
    auth: &NewSourceAuth<'_>,
    now: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO po_source_authorizations (id, org_id, source, key_id, max_cents, owner, authorized_by, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(id)
    .bind(org)
    .bind(auth.source)
    .bind(auth.key_id)
    .bind(auth.max_cents)
    .bind(auth.owner)
    .bind(auth.by)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    for vendor_id in auth.vendor_ids {
        sqlx::query(
            "INSERT INTO po_source_authorization_vendors (org_id, authorization_id, vendor_id) VALUES ($1,$2,$3)",
        )
        .bind(org)
        .bind(id)
        .bind(vendor_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(id)
}

/// `Ok(Some(true))` when this call revoked it. `Ok(Some(false))` when it was already revoked.
/// `Ok(None)` when the id is unknown.
pub async fn revoke_source_auth(
    tx: &mut Tx,
    id: Uuid,
    by: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<bool>> {
    let updated = sqlx::query(
        "UPDATE po_source_authorizations SET revoked_at = $2, revoked_by = $3 WHERE id = $1 AND revoked_at IS NULL RETURNING id",
    )
    .bind(id)
    .bind(now)
    .bind(by)
    .fetch_optional(&mut **tx)
    .await?;
    if updated.is_some() {
        return Ok(Some(true));
    }
    Ok(source_auth(tx, id).await?.map(|_| false))
}

#[derive(Debug, Clone)]
pub struct VendorLink {
    pub vendor_id: Uuid,
    pub source: String,
    pub external_id: String,
}

pub async fn vendor_links_touching(
    tx: &mut Tx,
    source: &str,
    vendor_id: Uuid,
    external_id: &str,
) -> anyhow::Result<Vec<VendorLink>> {
    let rows = sqlx::query(
        "SELECT vendor_id, source, external_id FROM vendor_external_ids \
         WHERE source = $1 AND (vendor_id = $2 OR external_id = $3)",
    )
    .bind(source)
    .bind(vendor_id)
    .bind(external_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .iter()
        .map(|r| VendorLink {
            vendor_id: r.get("vendor_id"),
            source: r.get("source"),
            external_id: r.get("external_id"),
        })
        .collect())
}

pub async fn insert_vendor_link(
    tx: &mut Tx,
    org: Uuid,
    vendor_id: Uuid,
    source: &str,
    external_id: &str,
    by: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO vendor_external_ids (id, org_id, vendor_id, source, external_id, linked_by, linked_at) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::new_v4())
    .bind(org)
    .bind(vendor_id)
    .bind(source)
    .bind(external_id)
    .bind(by)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
