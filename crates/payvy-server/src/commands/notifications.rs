//! Durable workflow notices. Claim before the non-idempotent email call; no
//! company/bill/role locks survive the call. Uncertain sends are never replayed.
use super::{CommandError, Cx, bills};
use crate::mailer::{DeliveryFailure, Mail};
use chrono::Duration;
use payvy_store::{self as store, Notification};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configure {
    enabled: bool,
    reminders: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillInput {
    bill_id: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deliver {
    notification_id: Uuid,
}

pub async fn settings(cx: &mut Cx<'_>, _: Empty) -> Result<Value, CommandError> {
    Ok(
        json!({"settings": store::notification_settings(&mut cx.tx, cx.org).await?,
        "sender": match cx.auth { None => "unconfigured", Some(a) if a.mailer.is_log() => "log", Some(_) => "email" },
        "reminder_after_hours":48,"reminder_interval_hours":24,"max_reminders":3 }),
    )
}
pub async fn configure(cx: &mut Cx<'_>, input: Configure) -> Result<Value, CommandError> {
    if input.enabled {
        auth(cx)?;
    }
    store::lock_organization(&mut cx.tx, cx.org).await?;
    let old = store::notification_settings(&mut cx.tx, cx.org).await?;
    if old.enabled == input.enabled && old.reminders == input.reminders {
        return Err(CommandError::Conflict(
            "notification settings are already the same".into(),
        ));
    }
    store::set_notification_settings(
        &mut cx.tx,
        cx.org,
        input.enabled,
        input.reminders,
        cx.now,
        &cx.actor.name,
    )
    .await?;
    cx.emit(
        "notification.settings_changed",
        cx.org,
        json!({"from":old,"to":{"enabled":input.enabled,"reminders":input.reminders}}),
    )
    .await?;
    settings(cx, Empty {}).await
}
fn auth<'a>(cx: &Cx<'a>) -> Result<&'a crate::auth::Auth, CommandError> {
    let auth = cx.auth.ok_or(CommandError::NotConfigured(
        "configure PAYVY_PUBLIC_URL and an email sender first",
    ))?;
    let url = reqwest::Url::parse(&auth.public_url)
        .map_err(|_| CommandError::Invalid("invalid public web URL".into()))?;
    if url.scheme() != "https"
        && !(cfg!(debug_assertions)
            && url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1")))
    {
        return Err(CommandError::Invalid(
            "notification links need an HTTPS public web URL".into(),
        ));
    }
    Ok(auth)
}
pub async fn list(cx: &mut Cx<'_>, input: BillInput) -> Result<Value, CommandError> {
    store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    Ok(json!({"notifications":store::notification_history(&mut cx.tx,input.bill_id).await?}))
}
pub async fn queue(cx: &mut Cx<'_>, input: BillInput) -> Result<Value, CommandError> {
    let settings = store::notification_settings(&mut cx.tx, cx.org).await?;
    if !settings.enabled {
        return Err(CommandError::Conflict(
            "company notifications are off".into(),
        ));
    }
    auth(cx)?;
    let (bill, step, action, recipients) = bills::notification_targets(cx, input.bill_id).await?;
    let mut queued = vec![];
    for person in recipients {
        let latest = store::notification_latest(&mut cx.tx, bill.id, &step, person.id).await?;
        let reminder = match latest {
            None => 0,
            Some(n)
                if settings.reminders
                    && action == "approve"
                    && n.reminder < 3
                    && matches!(n.status.as_str(), "sent" | "logged")
                    && n.finished_at.is_some_and(|t| {
                        t + Duration::hours(if n.reminder == 0 { 48 } else { 24 }) <= cx.now
                    }) =>
            {
                n.reminder + 1
            }
            _ => continue,
        };
        let n = Notification {
            id: Uuid::new_v4(),
            bill_id: bill.id,
            person_id: person.id,
            actor: person.actor,
            email: person.email,
            step_id: step.clone(),
            action: action.clone(),
            reminder,
            status: "queued".into(),
            attempts: 0,
            created_at: cx.now,
            next_attempt_at: cx.now,
            attempted_at: None,
            finished_at: None,
            reason: None,
        };
        if store::insert_notification(&mut cx.tx, cx.org, &n).await? {
            cx.emit("notification.queued",bill.id,json!({"notification_id":n.id,"actor":n.actor,"step_id":step,"action":action,"reminder":reminder})).await?;
            queued.push(n.id);
        }
    }
    if queued.is_empty() {
        return Err(CommandError::Conflict(
            "no new notifications are due for this bill".into(),
        ));
    }
    Ok(json!({"queued":queued}))
}
async fn finish(
    cx: &mut Cx<'_>,
    n: &Notification,
    status: &str,
    reason: Option<&str>,
    retry: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Value, CommandError> {
    store::finish_notification(&mut cx.tx, n.id, status, reason, (cx.clock)(), retry).await?;
    cx.emit("notification.delivery",n.bill_id,json!({"notification_id":n.id,"actor":n.actor,"step_id":n.step_id,"reminder":n.reminder,"status":status,"reason":reason,"retry_at":retry})).await?;
    Ok(json!({"notification":store::notification_get(&mut cx.tx,n.id).await?}))
}
fn line(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(250).collect()
}
pub async fn deliver(cx: &mut Cx<'_>, input: Deliver) -> Result<Value, CommandError> {
    let n = store::notification_get(&mut cx.tx, input.notification_id)
        .await?
        .ok_or(CommandError::NotFound("notification"))?;
    if n.status == "sending" {
        let n = store::notification_lock(&mut cx.tx, n.id)
            .await?
            .ok_or(CommandError::NotFound("notification"))?;
        if n.status == "sending"
            && n.attempted_at
                .is_some_and(|t| t + Duration::minutes(5) <= cx.now)
        {
            return finish(cx, &n, "unknown", Some("interrupted_send"), None).await;
        }
        return Err(CommandError::Conflict(
            "notification is already being delivered".into(),
        ));
    }
    let settings = store::notification_settings(&mut cx.tx, cx.org).await?;
    if !settings.enabled {
        return Err(CommandError::Conflict(
            "company notifications are off".into(),
        ));
    }
    let auth = auth(cx)?;
    // Same lock order as queue: bill, then notification. Recheck all eligibility
    // at send time, including roles, removed people, and the current step.
    let (bill, step, action, recipients) = bills::notification_targets(cx, n.bill_id).await?;
    let n = store::notification_lock(&mut cx.tx, n.id)
        .await?
        .ok_or(CommandError::NotFound("notification"))?;
    if n.status != "queued" || n.next_attempt_at > cx.now {
        return Err(CommandError::Conflict(
            "notification is not due for delivery".into(),
        ));
    }
    let person = recipients
        .iter()
        .find(|p| p.id == n.person_id && p.email == n.email && p.actor == n.actor);
    if person.is_none()
        || step != n.step_id
        || action != n.action
        || (n.reminder > 0 && !settings.reminders)
    {
        return finish(cx, &n, "cancelled", Some("no_longer_actionable"), None).await;
    }
    let company = store::organization(&mut cx.tx, cx.org)
        .await?
        .ok_or(CommandError::NotFound("organization"))?;
    let label = if action == "approve" {
        "Approval needed"
    } else {
        "Ready for payment release"
    };
    let subject = format!(
        "{}{} · Payvy",
        if n.reminder > 0 { "Reminder: " } else { "" },
        label
    );
    let text = format!(
        "{}\n\n{} needs your {} for invoice {} from {} (${}.{:02}).\n{}\n\nOpen the bill and sign in to review it:\n{}/bills/{}\n\nThis email does not approve or pay the bill. Its current workflow and your access determine what you can do.\n",
        label,
        line(&company.name),
        if action == "approve" {
            "approval"
        } else {
            "payment release"
        },
        line(&bill.invoice_number),
        line(&bill.vendor_name),
        bill.amount_cents / 100,
        bill.amount_cents % 100,
        bill.due_date
            .map(|d| format!("Due {d}."))
            .unwrap_or_default(),
        auth.public_url,
        bill.id
    );
    if cx.preview {
        return Ok(json!({"notification":n,"subject":subject,"would_send":true}));
    }
    store::claim_notification(&mut cx.tx, n.id, cx.now).await?;
    cx.emit(
        "notification.sending",
        bill.id,
        json!({"notification_id":n.id,"attempt":n.attempts+1}),
    )
    .await?;
    let next = cx.store.begin(cx.org).await?;
    let previous = std::mem::replace(&mut cx.tx, next);
    previous.commit().await.map_err(anyhow::Error::from)?;
    // No locks across HTTP. A role or workflow change after this checkpoint
    // cannot retract mail in flight; the linked app always checks fresh access.
    let result = auth
        .mailer
        .deliver(Mail {
            to: n.email.clone(),
            subject,
            text,
        })
        .await;
    let current = store::notification_lock(&mut cx.tx, n.id)
        .await?
        .ok_or(CommandError::NotFound("notification"))?;
    if current.status != "sending" {
        return Err(CommandError::Conflict(
            "delivery has already been settled".into(),
        ));
    }
    let (status, reason, retry) = match result {
        Ok(()) => (
            if auth.mailer.is_log() {
                "logged"
            } else {
                "sent"
            },
            None,
            None,
        ),
        Err(DeliveryFailure::RateLimited) if current.attempts < 5 => (
            "queued",
            Some("rate_limited"),
            Some((cx.clock)() + Duration::minutes(1_i64 << current.attempts)),
        ),
        Err(DeliveryFailure::RateLimited) => ("failed", Some("rate_limit_exhausted"), None),
        Err(DeliveryFailure::Refused) => ("failed", Some("provider_refused"), None),
        Err(DeliveryFailure::Uncertain) => ("unknown", Some("provider_uncertain"), None),
    };
    finish(cx, &current, status, reason, retry).await
}
