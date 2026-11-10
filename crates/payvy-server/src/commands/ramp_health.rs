//! Recorded maintenance of an already-authorized grant. It cannot authorize,
//! activate, change the selected account, accept a payee, or send a payment.
use super::{
    CommandError, Cx,
    ramp::{Credentials, Renew, bank_purpose, config},
};
use chrono::{DateTime, Duration, Utc};
use payvy_pay::ramp::Error;
use payvy_store::{self as store, ramp as db};
use serde_json::{Value, json};

const RENEW_EARLY: Duration = Duration::minutes(5);
const CHECK_EVERY: Duration = Duration::minutes(15);
const MIN_INTERVAL: Duration = Duration::minutes(1);
const MAX_FAILURES: i32 = 5;

pub(super) fn connected(now: DateTime<Utc>, expires: DateTime<Utc>) -> db::Health {
    db::Health {
        health_status: "connected".into(),
        health_reason: None,
        access_expires_at: Some(expires),
        checked_at: Some(now),
        next_check_at: Some(
            (expires - RENEW_EARLY)
                .min(now + CHECK_EVERY)
                .max(now + MIN_INTERVAL),
        ),
        next_attempt_at: Some(now + MIN_INTERVAL),
        failures: 0,
    }
}
fn reconnect(health: &mut db::Health, reason: &str) {
    health.health_status = "needs_reconnection".into();
    health.health_reason = Some(reason.into());
    health.next_check_at = None;
    health.next_attempt_at = None;
}
fn failure(health: &mut db::Health, error: Error, now: DateTime<Utc>, renewing: bool) {
    match error {
        Error::Unauthorized => reconnect(health, "authorization_rejected"),
        Error::Rejected { status: 400 | 403 } if renewing => {
            reconnect(health, "authorization_rejected")
        }
        Error::InvalidInput => reconnect(health, "configuration_changed"),
        error => {
            health.failures = (health.failures + 1).min(MAX_FAILURES);
            let (reason, seconds) = match error {
                Error::RateLimited {
                    retry_after_seconds,
                } => (
                    "rate_limited",
                    retry_after_seconds.unwrap_or(60).clamp(60, 86400),
                ),
                Error::InvalidResponse => ("invalid_response", 60u64 << (health.failures - 1)),
                _ => ("provider_unavailable", 60u64 << (health.failures - 1)),
            };
            // At most five automatic failures per recovery cycle. Manual check
            // can resume after cooldown; it cannot bypass a Retry-After delay.
            let retry = now + Duration::seconds(seconds as i64);
            health.health_status = if health.failures >= MAX_FAILURES {
                "paused"
            } else {
                "retrying"
            }
            .into();
            health.health_reason = Some(reason.into());
            health.next_attempt_at = Some(retry);
            health.next_check_at = (health.failures < MAX_FAILURES).then_some(retry);
        }
    }
}
fn message(status: &str, reason: Option<&str>, expired: bool) -> &'static str {
    match (status, reason) {
        (_, Some("configuration_changed")) => {
            "Ramp settings changed on this server. Ask an administrator to check them, then reconnect Ramp."
        }
        (_, Some("credentials_unreadable")) => {
            "Ramp credentials could not be opened. Ask an administrator to check the encryption key, then reconnect Ramp."
        }
        (_, Some("business_changed")) => {
            "Ramp returned a different or inactive business. Reconnect and review the business before using this connection."
        }
        (_, Some("grant_expired")) => {
            "Ramp authorization expired. Reconnect Ramp to restore access."
        }
        (_, Some("no_refresh_grant")) => {
            "Ramp did not grant refresh access. Enable refresh access for the app, then reconnect Ramp."
        }
        ("needs_reconnection", _) => {
            "Ramp rejected the authorization. Reconnect Ramp to restore access."
        }
        ("paused", _) => {
            "Automatic checks paused after repeated failures. Check the connection again after the retry time."
        }
        ("retrying", Some("rate_limited")) => {
            "Ramp asked Payvy to wait. Access will be checked again after the retry time."
        }
        ("retrying", None) if expired => {
            "Ramp access expired. Payvy is checking whether it can be renewed."
        }
        ("retrying", _) => "Ramp could not be reached reliably. Payvy will retry automatically.",
        (_, _) if expired => "Ramp access expired. Payvy is checking whether it can be renewed.",
        ("connected", _) => {
            "Payvy renews access automatically when refresh access is available. Ramp payments remain disabled."
        }
        _ => "Payvy is checking this connection. Ramp payments remain disabled.",
    }
}
fn view(health: &db::Health, now: DateTime<Utc>) -> Value {
    let expired = health
        .access_expires_at
        .is_some_and(|d| d <= now + Duration::seconds(30));
    let status = if expired && health.health_status == "connected" {
        "retrying"
    } else {
        &health.health_status
    };
    json!({"status":status,"reason":health.health_reason,"message":message(status,health.health_reason.as_deref(),expired),
        "access_expired":expired,"access_expires_at":health.access_expires_at,"checked_at":health.checked_at,
        "next_check_at":health.next_check_at,"retry_at":health.next_attempt_at,
        "can_check":health.health_status != "needs_reconnection" && health.next_attempt_at.is_none_or(|d| d <= now),
        "payments_enabled":false})
}
pub(super) async fn get(cx: &mut Cx<'_>, c: &store::BankConnection) -> Result<Value, CommandError> {
    let mut health = db::health(&mut cx.tx, c.id)
        .await?
        .ok_or(CommandError::NotFound("Ramp connection health"))?;
    let (app, _) = db::details(&mut cx.tx, c.id)
        .await?
        .ok_or(CommandError::NotFound("Ramp connection details"))?;
    if config(cx).is_err()
        || config(cx)
            .is_ok_and(|conf| conf.client_id != app || conf.environment_name() != c.environment)
    {
        reconnect(&mut health, "configuration_changed");
    }
    Ok(view(&health, cx.now))
}

pub async fn maintain(cx: &mut Cx<'_>, input: Renew, manual: bool) -> Result<Value, CommandError> {
    store::lock_bank(&mut cx.tx, cx.org).await?;
    cx.now = (cx.clock)();
    let c = db::lock_connection(&mut cx.tx, input.connection_id)
        .await?
        .filter(|c| c.provider == "ramp" && c.retired_at.is_none())
        .ok_or(CommandError::NotFound("live Ramp connection"))?;
    let (app, details) = db::details(&mut cx.tx, c.id)
        .await?
        .ok_or(CommandError::NotFound("Ramp connection details"))?;
    let mut health = db::health(&mut cx.tx, c.id)
        .await?
        .ok_or(CommandError::NotFound("Ramp connection health"))?;
    if health.health_status == "needs_reconnection" {
        return Err(CommandError::Conflict(
            "Reconnect Ramp to restore access".into(),
        ));
    }
    if health.next_attempt_at.is_some_and(|d| d > cx.now)
        || (!manual
            && (health.health_status == "paused"
                || health.next_check_at.is_some_and(|d| d > cx.now)))
    {
        return Err(CommandError::Conflict(
            "Ramp connection is not due for another check yet".into(),
        ));
    }
    if cx.preview {
        return Ok(
            json!({"connection_id":c.id,"health":view(&health,cx.now),"note":"Preview does not check or renew credentials"}),
        );
    }
    if manual && health.health_status == "paused" {
        health.failures = 0;
    }
    health.checked_at = Some(cx.now);
    let changed_config = config(cx).is_err()
        || config(cx)
            .is_ok_and(|conf| conf.client_id != app || conf.environment_name() != c.environment);
    let renewed = if changed_config {
        reconnect(&mut health, "configuration_changed");
        false
    } else if let Ok(mut credentials) = Credentials::open(cx, &bank_purpose(c.id), &c.token_sealed)
    {
        health.access_expires_at = Some(credentials.access_expires);
        let due = credentials.access_expires <= cx.now + RENEW_EARLY;
        if due && credentials.refresh_expires.is_some_and(|d| d <= cx.now) {
            reconnect(&mut health, "grant_expired");
            false
        } else if due && credentials.refresh.is_none() {
            reconnect(&mut health, "no_refresh_grant");
            false
        } else {
            let mut renewed = false;
            let mut usable = true;
            if due {
                let response = config(cx)?
                    .oauth()?
                    .refresh(credentials.refresh.as_deref().unwrap())
                    .await;
                // Retry-After is relative to the response, not the time before
                // waiting for a lock or making a slow HTTP request.
                cx.now = (cx.clock)();
                match response {
                    Ok(tokens) => match Credentials::from_tokens(tokens, cx.now) {
                        Ok(mut next) => {
                            // Ramp third-party grants keep the original refresh credential and deadline.
                            next.refresh = credentials.refresh;
                            next.refresh_expires = credentials.refresh_expires;
                            let sealed = next.seal(cx, &bank_purpose(c.id))?;
                            db::update_credentials(&mut cx.tx, c.id, &sealed).await?;
                            health.access_expires_at = Some(next.access_expires);
                            credentials = next;
                            renewed = true;
                        }
                        Err(_) => {
                            reconnect(&mut health, "authorization_rejected");
                            usable = false;
                        }
                    },
                    Err(error) => {
                        failure(&mut health, error, cx.now, true);
                        usable = false;
                    }
                }
            }
            if usable {
                let response = config(cx)?.client(&credentials.access)?.business().await;
                cx.now = (cx.clock)();
                match response {
                    Ok(business)
                        if business.active && details["business_id"] == business.id.to_string() =>
                    {
                        health = connected(cx.now, credentials.access_expires);
                    }
                    Ok(_) => reconnect(&mut health, "business_changed"),
                    Err(error) => failure(&mut health, error, cx.now, false),
                }
            }
            renewed
        }
    } else {
        reconnect(&mut health, "credentials_unreadable");
        false
    };
    health.checked_at = Some(cx.now);
    db::set_health(&mut cx.tx, c.id, &health).await?;
    if renewed {
        cx.emit(
            "bank.credentials_renewed",
            c.id,
            json!({"provider":"ramp","connection_id":c.id}),
        )
        .await?;
    }
    cx.emit(
        "bank.connection_checked",
        c.id,
        json!({"provider":"ramp","connection_id":c.id,
        "status":health.health_status,"reason":health.health_reason,"renewed":renewed}),
    )
    .await?;
    Ok(
        json!({"connection_id":c.id,"health":view(&health,cx.now),"renewed":renewed,"access_expires_at":health.access_expires_at}),
    )
}
