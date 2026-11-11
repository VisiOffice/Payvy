//! The invoice stack: every copy of an invoice received for one bill.
//! Every query runs inside the caller's org-scoped transaction (RLS).

use chrono::{DateTime, Utc};
use payvy_domain::Cents;
use payvy_domain::bills::Line;
use serde::Serialize;
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::Tx;

#[derive(Debug, Clone, Serialize)]
pub struct Arrival {
    pub id: Uuid,
    pub bill_id: Uuid,
    pub document_id: Option<Uuid>,
    pub amount_cents: Cents,
    pub invoice_number: String,
    #[serde(skip)]
    pub lines: Vec<Line>,
    pub received_by: String,
    pub received_at: DateTime<Utc>,
    /// none | pending | accepted | rejected | superseded
    pub decision: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

fn arrival(r: &PgRow) -> anyhow::Result<Arrival> {
    let lines: serde_json::Value = r.get("lines");
    Ok(Arrival {
        id: r.get("id"),
        bill_id: r.get("bill_id"),
        document_id: r.get("document_id"),
        amount_cents: r.get("amount_cents"),
        invoice_number: r.get("invoice_number"),
        lines: serde_json::from_value(lines)?,
        received_by: r.get("received_by"),
        received_at: r.get("received_at"),
        decision: r.get("decision"),
        decided_by: r.get("decided_by"),
        decided_at: r.get("decided_at"),
    })
}

macro_rules! arrival_cols {
    () => {
        "id, bill_id, document_id, amount_cents, invoice_number, \
         lines, received_by, received_at, decision, decided_by, decided_at"
    };
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_arrival(
    tx: &mut Tx,
    org_id: Uuid,
    bill_id: Uuid,
    document_id: Option<Uuid>,
    amount_cents: Cents,
    invoice_number: &str,
    lines: &[Line],
    received_by: &str,
    received_at: DateTime<Utc>,
    decision: &str,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO bill_arrivals (id, org_id, bill_id, document_id, amount_cents, \
         invoice_number, lines, received_by, received_at, decision) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(org_id)
    .bind(bill_id)
    .bind(document_id)
    .bind(amount_cents)
    .bind(invoice_number)
    .bind(serde_json::to_value(lines)?)
    .bind(received_by)
    .bind(received_at)
    .bind(decision)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// The stack, oldest to newest.
pub async fn list_arrivals(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Vec<Arrival>> {
    let rows = sqlx::query(concat!(
        "SELECT ",
        arrival_cols!(),
        " FROM bill_arrivals WHERE bill_id = $1 ORDER BY seq"
    ))
    .bind(bill_id)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(arrival).collect()
}

/// The revision a person still has to decide, if any: the latest pending
/// arrival. At most one is pending; a newer revision supersedes the old.
pub async fn pending_revision(tx: &mut Tx, bill_id: Uuid) -> anyhow::Result<Option<Arrival>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        arrival_cols!(),
        " FROM bill_arrivals WHERE bill_id = $1 AND decision = 'pending' \
         ORDER BY seq DESC LIMIT 1"
    ))
    .bind(bill_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(arrival).transpose()
}

pub async fn supersede_pending(
    tx: &mut Tx,
    bill_id: Uuid,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE bill_arrivals SET decision = 'superseded', decided_at = $2 \
         WHERE bill_id = $1 AND decision = 'pending'",
    )
    .bind(bill_id)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn decide_arrival(
    tx: &mut Tx,
    arrival_id: Uuid,
    decision: &str,
    by: &str,
    at: DateTime<Utc>,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE bill_arrivals SET decision = $2, decided_by = $3, decided_at = $4 WHERE id = $1",
    )
    .bind(arrival_id)
    .bind(decision)
    .bind(by)
    .bind(at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Makes an accepted revision the payable version: the bill takes the
/// arrival's amount and lines. The prior version stays in the stack.
pub async fn set_bill_amount(
    tx: &mut Tx,
    org_id: Uuid,
    bill_id: Uuid,
    amount_cents: Cents,
    lines: &[Line],
) -> anyhow::Result<()> {
    sqlx::query("UPDATE bills SET amount_cents = $2 WHERE id = $1")
        .bind(bill_id)
        .bind(amount_cents)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM bill_lines WHERE bill_id = $1")
        .bind(bill_id)
        .execute(&mut **tx)
        .await?;
    for (i, line) in lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO bill_lines (id, org_id, bill_id, position, description, amount_cents, account) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::new_v4())
        .bind(org_id)
        .bind(bill_id)
        .bind(i as i32)
        .bind(&line.description)
        .bind(line.amount_cents)
        .bind(&line.account)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// (bill, times received, pending revision amount) for many bills at once.
pub async fn arrival_summaries(
    tx: &mut Tx,
    bill_ids: &[Uuid],
) -> anyhow::Result<Vec<(Uuid, i64, Option<Cents>)>> {
    let rows = sqlx::query(
        "SELECT bill_id, count(*) AS received_count, \
         (SELECT a.amount_cents FROM bill_arrivals a \
           WHERE a.bill_id = bill_arrivals.bill_id AND a.decision = 'pending' \
           ORDER BY a.seq DESC LIMIT 1) AS pending_amount \
         FROM bill_arrivals WHERE bill_id = ANY($1) GROUP BY bill_id",
    )
    .bind(bill_ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                r.get("bill_id"),
                r.get("received_count"),
                r.get("pending_amount"),
            )
        })
        .collect())
}
