use crate::Tx;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;

#[derive(Debug, Default, Serialize)]
pub struct NotificationSettings {
    pub enabled: bool,
    pub reminders: bool,
    pub updated_at: Option<DateTime<Utc>>,
    pub updated_by: Option<String>,
}
pub async fn notification_settings(tx: &mut Tx, org: Uuid) -> anyhow::Result<NotificationSettings> {
    let row = sqlx::query("SELECT enabled, reminders, updated_at, updated_by FROM notification_settings WHERE org_id = $1")
        .bind(org).fetch_optional(&mut **tx).await?;
    Ok(match row {
        Some(r) => NotificationSettings {
            enabled: r.get("enabled"),
            reminders: r.get("reminders"),
            updated_at: r.get("updated_at"),
            updated_by: r.get("updated_by"),
        },
        None => NotificationSettings {
            reminders: true,
            ..Default::default()
        },
    })
}
pub async fn set_notification_settings(
    tx: &mut Tx,
    org: Uuid,
    enabled: bool,
    reminders: bool,
    now: DateTime<Utc>,
    actor: &str,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO notification_settings (org_id,enabled,reminders,updated_at,updated_by) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (org_id) DO UPDATE SET enabled=$2, reminders=$3, updated_at=$4, updated_by=$5")
        .bind(org).bind(enabled).bind(reminders).bind(now).bind(actor).execute(&mut **tx).await?;
    Ok(())
}
#[derive(Debug, Serialize)]
pub struct Notification {
    pub id: Uuid,
    pub bill_id: Uuid,
    pub person_id: Uuid,
    pub actor: String,
    pub email: String,
    pub step_id: String,
    pub action: String,
    pub reminder: i32,
    pub status: String,
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
    pub next_attempt_at: DateTime<Utc>,
    pub attempted_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}
fn notification(r: PgRow) -> Notification {
    Notification {
        id: r.get("id"),
        bill_id: r.get("bill_id"),
        person_id: r.get("person_id"),
        actor: r.get("actor"),
        email: r.get("email"),
        step_id: r.get("step_id"),
        action: r.get("action"),
        reminder: r.get("reminder"),
        status: r.get("status"),
        attempts: r.get("attempts"),
        created_at: r.get("created_at"),
        next_attempt_at: r.get("next_attempt_at"),
        attempted_at: r.get("attempted_at"),
        finished_at: r.get("finished_at"),
        reason: r.get("reason"),
    }
}
pub async fn notification_history(tx: &mut Tx, bill: Uuid) -> anyhow::Result<Vec<Notification>> {
    Ok(sqlx::query(
        "SELECT * FROM notifications WHERE bill_id=$1 ORDER BY created_at DESC, id DESC LIMIT 500",
    )
    .bind(bill)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(notification)
    .collect())
}
pub async fn notification_latest(
    tx: &mut Tx,
    bill: Uuid,
    step: &str,
    person: Uuid,
) -> anyhow::Result<Option<Notification>> {
    Ok(sqlx::query("SELECT * FROM notifications WHERE bill_id=$1 AND step_id=$2 AND person_id=$3 ORDER BY reminder DESC LIMIT 1")
        .bind(bill).bind(step).bind(person).fetch_optional(&mut **tx).await?.map(notification))
}
pub async fn notification_get(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Notification>> {
    Ok(sqlx::query("SELECT * FROM notifications WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .map(notification))
}
pub async fn notification_lock(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Notification>> {
    Ok(
        sqlx::query("SELECT * FROM notifications WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .map(notification),
    )
}
pub async fn insert_notification(tx: &mut Tx, org: Uuid, n: &Notification) -> anyhow::Result<bool> {
    Ok(sqlx::query("INSERT INTO notifications (id,org_id,bill_id,person_id,actor,email,step_id,action,reminder,status,created_at,next_attempt_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'queued',$10,$10) ON CONFLICT DO NOTHING")
        .bind(n.id).bind(org).bind(n.bill_id).bind(n.person_id).bind(&n.actor).bind(&n.email).bind(&n.step_id).bind(&n.action).bind(n.reminder).bind(n.created_at).execute(&mut **tx).await?.rows_affected() == 1)
}
pub async fn claim_notification(tx: &mut Tx, id: Uuid, now: DateTime<Utc>) -> anyhow::Result<()> {
    sqlx::query("UPDATE notifications SET status='sending', attempts=attempts+1, attempted_at=$2, reason=NULL WHERE id=$1")
        .bind(id).bind(now).execute(&mut **tx).await?;
    Ok(())
}
pub async fn finish_notification(
    tx: &mut Tx,
    id: Uuid,
    status: &str,
    reason: Option<&str>,
    now: DateTime<Utc>,
    retry: Option<DateTime<Utc>>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE notifications SET status=$2, reason=$3, finished_at=$4, next_attempt_at=COALESCE($5,next_attempt_at) WHERE id=$1")
        .bind(id).bind(status).bind(reason).bind(if retry.is_some() {None} else {Some(now)}).bind(retry).execute(&mut **tx).await?;
    Ok(())
}
pub async fn notifications_due(tx: &mut Tx, now: DateTime<Utc>) -> anyhow::Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar("SELECT id FROM notifications WHERE (status='queued' AND next_attempt_at <= $1) OR (status='sending' AND attempted_at <= $1 - interval '5 minutes') ORDER BY next_attempt_at,id LIMIT 50")
        .bind(now).fetch_all(&mut **tx).await?)
}
/// Keyset pagination: no permanent starvation when a company has many bills.
pub async fn notification_bill_page(tx: &mut Tx, after: Option<Uuid>) -> anyhow::Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar("SELECT id FROM bills WHERE state='open' AND ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT 100")
        .bind(after).fetch_all(&mut **tx).await?)
}
