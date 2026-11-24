//! Cards on file (`vendor_cards`) and the charges the bank reports on them
//! (`card_charges`). Every query runs inside the caller's org-scoped
//! transaction (RLS).

use chrono::{DateTime, NaiveDate, Utc};
use payvy_domain::Cents;
use serde::Serialize;
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::Tx;

#[derive(Debug, Clone, Serialize)]
pub struct VendorCard {
    pub id: Uuid,
    pub vendor_id: Uuid,
    pub connection_id: Uuid,
    pub card_id: String,
    pub last_four: String,
    pub nickname: Option<String>,
    pub kind: String,
    pub charges_from: NaiveDate,
    pub status: String,
    pub reason: Option<String>,
    pub linked_by: String,
    pub linked_at: DateTime<Utc>,
    pub removed_by: Option<String>,
    pub removed_at: Option<DateTime<Utc>>,
}

macro_rules! card_cols {
    () => {
        "id, vendor_id, connection_id, card_id, last_four, nickname, kind, charges_from, status, reason, linked_by, linked_at, removed_by, removed_at"
    };
}

fn card(r: &PgRow) -> VendorCard {
    VendorCard {
        id: r.get("id"),
        vendor_id: r.get("vendor_id"),
        connection_id: r.get("connection_id"),
        card_id: r.get("card_id"),
        last_four: r.get("last_four"),
        nickname: r.get("nickname"),
        kind: r.get("kind"),
        charges_from: r.get("charges_from"),
        status: r.get("status"),
        reason: r.get("reason"),
        linked_by: r.get("linked_by"),
        linked_at: r.get("linked_at"),
        removed_by: r.get("removed_by"),
        removed_at: r.get("removed_at"),
    }
}

pub struct NewVendorCard<'a> {
    pub vendor_id: Uuid,
    pub connection_id: Uuid,
    pub card_id: &'a str,
    pub last_four: &'a str,
    pub nickname: Option<&'a str>,
    pub kind: &'a str,
    pub charges_from: NaiveDate,
    pub reason: Option<&'a str>,
    pub linked_by: &'a str,
    pub linked_at: DateTime<Utc>,
}

pub async fn insert_vendor_card(
    tx: &mut Tx,
    org_id: Uuid,
    new: &NewVendorCard<'_>,
) -> anyhow::Result<VendorCard> {
    let row = sqlx::query(concat!(
        "INSERT INTO vendor_cards (id, org_id, vendor_id, connection_id, card_id, last_four, nickname,
                                   kind, charges_from, status, reason, linked_by, linked_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'active', $10, $11, $12) RETURNING ",
        card_cols!()
    ))
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(new.vendor_id)
    .bind(new.connection_id)
    .bind(new.card_id)
    .bind(new.last_four)
    .bind(new.nickname)
    .bind(new.kind)
    .bind(new.charges_from)
    .bind(new.reason)
    .bind(new.linked_by)
    .bind(new.linked_at)
    .fetch_one(&mut **tx)
    .await?;
    Ok(card(&row))
}

/// The vendor's card on file, whatever connection it was linked under.
/// Changes to it take the company's bank lock (`lock_bank`).
pub async fn active_vendor_card(
    tx: &mut Tx,
    vendor_id: Uuid,
) -> anyhow::Result<Option<VendorCard>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        card_cols!(),
        " FROM vendor_cards WHERE vendor_id = $1 AND status = 'active'"
    ))
    .bind(vendor_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(card))
}

/// The vendor a card is on file with, at this connection.
pub async fn vendor_card_by_card(
    tx: &mut Tx,
    connection_id: Uuid,
    card_id: &str,
) -> anyhow::Result<Option<VendorCard>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        card_cols!(),
        " FROM vendor_cards WHERE connection_id = $1 AND card_id = $2 AND status = 'active'"
    ))
    .bind(connection_id)
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(card))
}

/// Every card on file at this connection.
pub async fn vendor_cards_at(tx: &mut Tx, connection_id: Uuid) -> anyhow::Result<Vec<VendorCard>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        card_cols!(),
        " FROM vendor_cards WHERE connection_id = $1 AND status = 'active' ORDER BY linked_at"
    ))
    .bind(connection_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(card).collect())
}

pub async fn remove_vendor_card(
    tx: &mut Tx,
    id: Uuid,
    by: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE vendor_cards SET status = 'removed', removed_by = $2, removed_at = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(by)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct CardCharge {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub vendor_card_id: Uuid,
    pub vendor_id: Uuid,
    pub vendor_name: String,
    pub last_four: String,
    pub transaction_id: String,
    pub amount_cents: Cents,
    pub status: String,
    pub counterparty: Option<String>,
    pub charged_at: DateTime<Utc>,
    pub posted_at: Option<DateTime<Utc>>,
    pub link: Option<String>,
    pub bill_id: Option<Uuid>,
    pub invoice_number: Option<String>,
    /// The paid bill's state: open (not yet cleared or not yet marked), paid, …
    pub bill_state: Option<String>,
    pub rule: Option<String>,
    pub matched_by: Option<String>,
    pub matched_at: Option<DateTime<Utc>>,
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
    pub dismissed_reason: Option<String>,
}

macro_rules! charge_select {
    () => {
        "SELECT c.id, c.connection_id, c.vendor_card_id, c.vendor_id, v.name AS vendor_name, k.last_four,
                c.transaction_id, c.amount_cents, c.status, c.counterparty, c.charged_at, c.posted_at,
                c.link, c.bill_id, b.invoice_number, b.state AS bill_state, c.rule, c.matched_by, c.matched_at,
                c.dismissed_by, c.dismissed_at, c.dismissed_reason
         FROM card_charges c
         JOIN vendors v ON v.id = c.vendor_id
         JOIN vendor_cards k ON k.id = c.vendor_card_id
         LEFT JOIN bills b ON b.id = c.bill_id "
    };
}

fn charge(r: &PgRow) -> CardCharge {
    CardCharge {
        id: r.get("id"),
        connection_id: r.get("connection_id"),
        vendor_card_id: r.get("vendor_card_id"),
        vendor_id: r.get("vendor_id"),
        vendor_name: r.get("vendor_name"),
        last_four: r.get("last_four"),
        transaction_id: r.get("transaction_id"),
        amount_cents: r.get("amount_cents"),
        status: r.get("status"),
        counterparty: r.get("counterparty"),
        charged_at: r.get("charged_at"),
        posted_at: r.get("posted_at"),
        link: r.get("link"),
        bill_id: r.get("bill_id"),
        invoice_number: r.get("invoice_number"),
        bill_state: r.get("bill_state"),
        rule: r.get("rule"),
        matched_by: r.get("matched_by"),
        matched_at: r.get("matched_at"),
        dismissed_by: r.get("dismissed_by"),
        dismissed_at: r.get("dismissed_at"),
        dismissed_reason: r.get("dismissed_reason"),
    }
}

pub struct SeenCharge<'a> {
    pub connection_id: Uuid,
    pub vendor_card_id: Uuid,
    pub vendor_id: Uuid,
    pub transaction_id: &'a str,
    pub amount_cents: Cents,
    pub status: &'a str,
    pub counterparty: Option<&'a str>,
    pub charged_at: DateTime<Utc>,
    pub posted_at: Option<DateTime<Utc>>,
    pub link: Option<&'a str>,
}

/// Records a charge as the bank reports it now. Returns the charge and the
/// status it had before (None when it is new). The vendor and card it was
/// first seen under stay, even if the card is later moved.
pub async fn record_charge(
    tx: &mut Tx,
    org_id: Uuid,
    seen: &SeenCharge<'_>,
    at: DateTime<Utc>,
) -> anyhow::Result<(Uuid, Option<String>)> {
    let before: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM card_charges WHERE connection_id = $1 AND transaction_id = $2 FOR UPDATE",
    )
    .bind(seen.connection_id)
    .bind(seen.transaction_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((id, status)) = before {
        sqlx::query(
            "UPDATE card_charges SET status = $2, amount_cents = $3, posted_at = $4,
                    link = COALESCE($5, link), counterparty = COALESCE($6, counterparty), updated_at = $7
             WHERE id = $1",
        )
        .bind(id)
        .bind(seen.status)
        .bind(seen.amount_cents)
        .bind(seen.posted_at)
        .bind(seen.link)
        .bind(seen.counterparty)
        .bind(at)
        .execute(&mut **tx)
        .await?;
        return Ok((id, Some(status)));
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO card_charges (id, org_id, connection_id, vendor_card_id, vendor_id, transaction_id,
                                   amount_cents, status, counterparty, charged_at, posted_at, link,
                                   created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13)",
    )
    .bind(id)
    .bind(org_id)
    .bind(seen.connection_id)
    .bind(seen.vendor_card_id)
    .bind(seen.vendor_id)
    .bind(seen.transaction_id)
    .bind(seen.amount_cents)
    .bind(seen.status)
    .bind(seen.counterparty)
    .bind(seen.charged_at)
    .bind(seen.posted_at)
    .bind(seen.link)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok((id, None))
}

/// Locks the charge for the rest of the transaction.
pub async fn get_charge(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<CardCharge>> {
    sqlx::query("SELECT 1 FROM card_charges WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let row = sqlx::query(concat!(charge_select!(), "WHERE c.id = $1"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.as_ref().map(charge))
}

/// The charge that paid this bill, if one did.
pub async fn charge_for_bill(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Option<CardCharge>> {
    let row = sqlx::query(concat!(charge_select!(), "WHERE c.bill_id = $1"))
        .bind(bill_id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(row.as_ref().map(charge))
}

/// Charges newest first. `open` keeps only the ones a person still has to
/// look at: not reversed, and paying no bill, or paying one not yet paid.
pub async fn list_charges(
    tx: &mut Tx,
    vendor_id: Option<Uuid>,
    open: bool,
    limit: i64,
) -> anyhow::Result<Vec<CardCharge>> {
    let rows = sqlx::query(concat!(
        charge_select!(),
        "WHERE ($1::uuid IS NULL OR c.vendor_id = $1)
           AND (NOT $2 OR (c.status <> 'reversed' AND c.dismissed_at IS NULL
                           AND (c.bill_id IS NULL OR b.state = 'open')))
         ORDER BY c.charged_at DESC, c.id LIMIT $3"
    ))
    .bind(vendor_id)
    .bind(open)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(charge).collect())
}

/// Posted charges of this vendor that pay no bill and weren't dismissed.
/// Locked, skipping ones another transaction is deciding.
pub async fn unmatched_posted_charges(
    tx: &mut Tx,
    vendor_id: Uuid,
) -> anyhow::Result<Vec<CardCharge>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM card_charges
         WHERE vendor_id = $1 AND status = 'posted' AND bill_id IS NULL AND dismissed_at IS NULL
         ORDER BY charged_at, id FOR UPDATE SKIP LOCKED",
    )
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?;
    let rows = sqlx::query(concat!(
        charge_select!(),
        "WHERE c.id = ANY($1) ORDER BY c.charged_at, c.id"
    ))
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(charge).collect())
}

/// Open bills of this vendor that no charge pays: (id, amount, invoice date).
pub async fn bills_awaiting_a_charge(
    tx: &mut Tx,
    vendor_id: Uuid,
) -> anyhow::Result<Vec<(Uuid, Cents, NaiveDate)>> {
    Ok(sqlx::query_as(
        "SELECT b.id, b.amount_cents, b.invoice_date FROM bills b
         WHERE b.vendor_id = $1 AND b.state = 'open'
           AND NOT EXISTS (SELECT 1 FROM card_charges c WHERE c.bill_id = b.id)
         ORDER BY b.invoice_date, b.id",
    )
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?)
}

/// Charges that pay an open bill: settled once the bill is cleared.
pub async fn charges_paying_open_bills(
    tx: &mut Tx,
    vendor_id: Uuid,
) -> anyhow::Result<Vec<CardCharge>> {
    let rows = sqlx::query(concat!(
        charge_select!(),
        "WHERE c.vendor_id = $1 AND c.bill_id IS NOT NULL AND c.status = 'posted' AND b.state = 'open'
         ORDER BY c.charged_at, c.id"
    ))
    .bind(vendor_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.iter().map(charge).collect())
}

pub async fn set_charge_bill(
    tx: &mut Tx,
    id: Uuid,
    bill_id: Option<Uuid>,
    rule: Option<&str>,
    by: Option<&str>,
    at: Option<DateTime<Utc>>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE card_charges SET bill_id = $2, rule = $3, matched_by = $4, matched_at = $5 WHERE id = $1",
    )
    .bind(id)
    .bind(bill_id)
    .bind(rule)
    .bind(by)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn dismiss_charge(
    tx: &mut Tx,
    id: Uuid,
    by: &str,
    reason: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE card_charges SET dismissed_by = $2, dismissed_reason = $3, dismissed_at = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(by)
    .bind(reason)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
