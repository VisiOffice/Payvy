use crate::Tx;
use chrono::{DateTime, Utc};
use payvy_domain::invoice_review::Review;
use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct InvoiceReview {
    pub id: Uuid,
    pub document_id: Uuid,
    pub extraction_id: Option<Uuid>,
    pub version: i32,
    pub arrangement_version: i32,
    pub payload: Review,
    pub summary_reason: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip)]
    pub request_payload: Value,
}
fn row(r: PgRow) -> anyhow::Result<InvoiceReview> {
    Ok(InvoiceReview {
        id: r.get("id"),
        document_id: r.get("document_id"),
        extraction_id: r.get("extraction_id"),
        version: r.get("version"),
        arrangement_version: r.get("arrangement_version"),
        payload: serde_json::from_value(r.get("payload"))?,
        summary_reason: r.get("summary_reason"),
        created_by: r.get("created_by"),
        created_at: r.get("created_at"),
        request_payload: r.get("request_payload"),
    })
}
/// Serialize enrollment/configuration with bill creation; released before any HTTP.
pub async fn lock_invoice_settings(tx: &mut Tx, org: Uuid) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 722))")
        .bind(org.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}
pub async fn invoice_review_enabled(tx: &mut Tx) -> anyhow::Result<bool> {
    Ok(sqlx::query("SELECT enabled FROM invoice_review_settings")
        .fetch_optional(&mut **tx)
        .await?
        .is_some_and(|r| r.get("enabled")))
}
pub async fn set_invoice_review_enabled(
    tx: &mut Tx,
    org: Uuid,
    enabled: bool,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO invoice_review_settings(org_id, enabled) VALUES ($1,$2) ON CONFLICT(org_id) DO UPDATE SET enabled=excluded.enabled").bind(org).bind(enabled).execute(&mut **tx).await?;
    Ok(())
}
pub async fn invoice_review_history(tx: &mut Tx, doc: Uuid) -> anyhow::Result<Vec<InvoiceReview>> {
    sqlx::query("SELECT * FROM invoice_reviews WHERE document_id=$1 ORDER BY version DESC")
        .bind(doc)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(row)
        .collect()
}
pub async fn invoice_review_replay(
    tx: &mut Tx,
    actor: &str,
    request: Uuid,
) -> anyhow::Result<Option<InvoiceReview>> {
    sqlx::query("SELECT * FROM invoice_reviews WHERE created_by=$1 AND request_id=$2")
        .bind(actor)
        .bind(request)
        .fetch_optional(&mut **tx)
        .await?
        .map(row)
        .transpose()
}
/// Every view of the same original stays enrolled, including split siblings
/// and an original restored by unsplit, even when new enrollment is off.
pub async fn invoice_review_enrolled(tx: &mut Tx, doc: Uuid) -> anyhow::Result<bool> {
    let r = sqlx::query("SELECT EXISTS(SELECT 1 FROM documents d JOIN documents source ON source.org_id=d.org_id AND source.sha256=d.sha256 JOIN invoice_reviews r ON r.org_id=source.org_id AND r.document_id=source.id WHERE d.id=$1) AS enrolled").bind(doc).fetch_one(&mut **tx).await?;
    Ok(r.get("enrolled"))
}
pub struct NewInvoiceReview<'a> {
    pub document_id: Uuid,
    pub extraction_id: Option<Uuid>,
    pub version: i32,
    pub arrangement_version: i32,
    pub payload: &'a Review,
    pub summary_reason: Option<&'a str>,
    pub actor: &'a str,
    pub now: DateTime<Utc>,
    pub request_id: Uuid,
    pub request_payload: &'a Value,
}
pub async fn insert_invoice_review(
    tx: &mut Tx,
    org: Uuid,
    n: NewInvoiceReview<'_>,
) -> anyhow::Result<InvoiceReview> {
    row(sqlx::query("INSERT INTO invoice_reviews (id, org_id, document_id, extraction_id, version, arrangement_version, payload, summary_reason, created_by, created_at, request_id, request_payload) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING *")
        .bind(Uuid::new_v4()).bind(org).bind(n.document_id).bind(n.extraction_id).bind(n.version).bind(n.arrangement_version).bind(serde_json::to_value(n.payload)?).bind(n.summary_reason).bind(n.actor).bind(n.now).bind(n.request_id).bind(n.request_payload).fetch_one(&mut **tx).await?)
}
pub async fn link_invoice_review(
    tx: &mut Tx,
    org: Uuid,
    bill: Uuid,
    review: Uuid,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO bill_invoice_reviews(org_id,bill_id,review_id) VALUES ($1,$2,$3)")
        .bind(org)
        .bind(bill)
        .bind(review)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
pub async fn bill_invoice_review(tx: &mut Tx, bill: Uuid) -> anyhow::Result<Option<InvoiceReview>> {
    sqlx::query("SELECT r.* FROM invoice_reviews r JOIN bill_invoice_reviews b ON b.review_id=r.id AND b.org_id=r.org_id WHERE b.bill_id=$1").bind(bill).fetch_optional(&mut **tx).await?.map(row).transpose()
}
