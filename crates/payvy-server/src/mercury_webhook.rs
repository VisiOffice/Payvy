//! Mercury's webhook. Connecting Mercury registers an endpoint for
//! transaction events at `/webhooks/mercury/{org}/{connection}`; Mercury
//! signs each delivery with that endpoint's secret:
//!
//! ```text
//! Mercury-Signature: t=<unix seconds>,v1=hex(HMAC-SHA256(secret, "{t}.{raw body}"))
//! ```
//!
//! A delivery is only a nudge. Payvy doesn't take a payment's state from it:
//! it asks Mercury about the payments the event may concern, through the
//! same `payment.refresh` the payment checker runs each minute. So a forged,
//! stale or duplicated event can at most cause an extra look, and a missed
//! one is caught by the checker.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, Mac};
use payvy_store as store;
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use crate::commands::{self, Mode};
use crate::{Actor, AppState, payment_worker};

/// Deliveries signed longer ago than this are refused (Mercury's advice).
const WINDOW_SECONDS: i64 = 5 * 60;

/// What a connection's webhook secret is sealed as.
pub fn sealed_as(connection: Uuid) -> String {
    format!("webhook:{connection}")
}

/// Mercury's signature header: `t=<seconds>,v1=<hex>` (several v1 may come
/// while a secret rotates).
pub fn verify(secret: &str, header: &str, body: &[u8], now: i64) -> Result<(), &'static str> {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", t)) => timestamp = t.parse::<i64>().ok(),
            Some(("v1", s)) => signatures.push(s),
            _ => {}
        }
    }
    let t = timestamp.ok_or("no timestamp in the signature")?;
    let ok = signatures.iter().any(|s| {
        let Ok(given) = hex::decode(s) else {
            return false;
        };
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
        mac.update(format!("{t}.").as_bytes());
        mac.update(body);
        mac.verify_slice(&given).is_ok()
    });
    if !ok {
        return Err("bad signature");
    }
    if (now - t).abs() > WINDOW_SECONDS {
        return Err("timestamp outside the 5-minute window");
    }
    Ok(())
}

/// Signs a body the way Mercury does. For tests and local fakes.
pub fn sign(secret: &str, t: i64, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key length");
    mac.update(format!("{t}.").as_bytes());
    mac.update(body);
    format!("t={t},v1={}", hex::encode(mac.finalize().into_bytes()))
}

fn reply(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "message": message }))).into_response()
}

pub async fn webhook(
    State(state): State<AppState>,
    Path((org, connection)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(bank) = state.bank.clone() else {
        return reply(StatusCode::NOT_FOUND, "banks aren't configured");
    };
    let Some(header) = headers
        .get("mercury-signature")
        .and_then(|v| v.to_str().ok())
    else {
        return reply(StatusCode::UNAUTHORIZED, "missing Mercury-Signature");
    };
    // 4xx answers are final at Mercury; 5xx are retried.
    let found = async {
        let mut tx = state.store.begin(org).await?;
        let c = store::get_connection(&mut tx, connection).await?;
        tx.rollback().await?;
        anyhow::Ok(c)
    }
    .await;
    let c = match found {
        Ok(Some(c)) => c,
        Ok(None) => return reply(StatusCode::NOT_FOUND, "no such connection"),
        Err(e) => {
            tracing::error!("mercury webhook: {e:#}");
            return reply(StatusCode::SERVICE_UNAVAILABLE, "try again later");
        }
    };
    let Some(sealed) = &c.webhook_secret_sealed else {
        return reply(StatusCode::NOT_FOUND, "this connection has no webhook");
    };
    let secret = match bank.sealer.open(org, &sealed_as(c.id), sealed) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(connection = %c.id, "mercury webhook secret: {e:#}");
            return reply(StatusCode::SERVICE_UNAVAILABLE, "try again later");
        }
    };
    if let Err(why) = verify(&secret, header, &body, (state.clock)().timestamp()) {
        tracing::warn!(connection = %c.id, "mercury webhook refused: {why}");
        return reply(StatusCode::UNAUTHORIZED, why);
    }
    let Ok(event) = serde_json::from_slice::<Value>(&body) else {
        return reply(StatusCode::BAD_REQUEST, "not JSON");
    };
    if event["resourceType"] != "transaction" {
        return reply(StatusCode::OK, "ignored");
    }
    let Some(transaction) = event["resourceId"].as_str().map(str::to_owned) else {
        return reply(StatusCode::OK, "ignored");
    };
    // Answer within Mercury's 5 seconds; ask Mercury afterwards.
    tokio::spawn(async move {
        if let Err(e) = refresh(&state, org, c.id, &transaction).await {
            tracing::warn!(connection = %c.id, "mercury webhook: {e:#}");
        }
    });
    reply(StatusCode::OK, "received")
}

/// Asks Mercury about every payment a transaction event may concern.
/// Returns how many changed.
pub async fn refresh(
    state: &AppState,
    org: Uuid,
    connection: Uuid,
    transaction: &str,
) -> anyhow::Result<usize> {
    let mut tx = state.store.begin(org).await?;
    let ids = store::payments_for_transaction(&mut tx, connection, transaction).await?;
    tx.rollback().await?;
    let mut changed = 0;
    for id in ids {
        let mode = Mode::Commit {
            confirm_token: None,
        };
        match commands::run(
            state,
            &Actor::agent(org, payment_worker::ACTOR),
            "payment.refresh",
            json!({ "payment_id": id }),
            mode,
        )
        .await
        {
            Ok(out) if out["result"]["changed"] == true => changed += 1,
            Ok(_) => {}
            Err(e) => tracing::warn!(payment = %id, "mercury webhook: {e}"),
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_match_mercurys_scheme() {
        let body = br#"{"id":"evt","resourceType":"transaction"}"#;
        let header = sign("whsec", 1_700_000_000, body);
        assert!(verify("whsec", &header, body, 1_700_000_100).is_ok());
        assert_eq!(
            verify("other", &header, body, 1_700_000_100),
            Err("bad signature")
        );
        assert_eq!(
            verify("whsec", &header, b"{}", 1_700_000_100),
            Err("bad signature")
        );
        assert_eq!(
            verify("whsec", &header, body, 1_700_000_000 + 301),
            Err("timestamp outside the 5-minute window")
        );
        // Several v1 values: any one may match.
        let rotated = header.replace("v1=", "v1=00,v1=");
        assert!(verify("whsec", &rotated, body, 1_700_000_000).is_ok());
    }

    #[test]
    fn known_vector() {
        // HMAC-SHA256("secret", "1.{}"), computed independently with openssl.
        assert_eq!(sign("secret", 1, b"{}"), format!("t=1,v1={}", KNOWN));
    }

    const KNOWN: &str = "1122767b193110cfec322b6f199b599edbf608ed087f2d27afb0b97d99523908";
}
