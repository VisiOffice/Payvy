//! The inbound email webhook. A mail front end (the Cloudflare Email Worker
//! in `workers/email-inbound`, or anything else that can make an HTTP
//! request) posts each raw message here, signed with PAYVY_INBOUND_SECRET:
//!
//! ```text
//! POST /webhooks/email
//! X-Payvy-Timestamp: <unix seconds>
//! X-Payvy-Recipient: <envelope recipient, e.g. k7f3q9x2m4p8@in.example.com>
//! X-Payvy-Signature: hex(HMAC-SHA256(secret, "{timestamp}.{recipient}.{hex(sha256(body))}"))
//! <body: the raw RFC 5322 message>
//! ```
//!
//! The response tells the sender whether to retry: 2xx means stop
//! (imported, a duplicate, an unknown address, or an email that can never be
//! read), 5xx means try again later.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use hmac::{Hmac, Mac};
use payvy_domain::email;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::commands::{self, CommandError, Mode};
use crate::{Actor, AppState};

/// Requests older or newer than this are refused, so a captured request
/// can't be replayed later. Replays inside it are caught as duplicates.
const WINDOW_SECONDS: i64 = 5 * 60;

pub struct Inbound {
    /// Addresses are `{token}@{domain}`, e.g. in.payvy.com.
    pub domain: String,
    secret: Vec<u8>,
}

impl Inbound {
    pub fn new(domain: &str, secret: &str) -> anyhow::Result<Self> {
        if secret.len() < 32 {
            anyhow::bail!("PAYVY_INBOUND_SECRET must be at least 32 characters");
        }
        Ok(Self {
            domain: domain.trim().to_ascii_lowercase(),
            secret: secret.as_bytes().to_vec(),
        })
    }

    pub fn sign(&self, timestamp: i64, recipient: &str, body: &[u8]) -> String {
        hex::encode(self.mac(timestamp, recipient, body).finalize().into_bytes())
    }

    fn verify(&self, timestamp: i64, recipient: &str, body: &[u8], signature: &str) -> bool {
        let Ok(given) = hex::decode(signature.trim()) else {
            return false;
        };
        self.mac(timestamp, recipient, body)
            .verify_slice(&given)
            .is_ok()
    }

    fn mac(&self, timestamp: i64, recipient: &str, body: &[u8]) -> Hmac<Sha256> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC takes any key length");
        let digest = hex::encode(Sha256::digest(body));
        mac.update(format!("{timestamp}.{recipient}.{digest}").as_bytes());
        mac
    }
}

pub async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(inbound) = state.inbound.as_deref() else {
        return reply(
            StatusCode::NOT_FOUND,
            "not_configured",
            "email intake isn't configured",
        );
    };
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    let (Some(ts), Some(recipient), Some(sig)) = (
        header("x-payvy-timestamp").and_then(|t| t.parse::<i64>().ok()),
        header("x-payvy-recipient"),
        header("x-payvy-signature"),
    ) else {
        return reply(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing signature headers",
        );
    };
    if !inbound.verify(ts, recipient, &body, sig) {
        return reply(StatusCode::UNAUTHORIZED, "unauthorized", "bad signature");
    }
    if ((state.clock)().timestamp() - ts).abs() > WINDOW_SECONDS {
        return reply(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "timestamp outside the 5-minute window",
        );
    }

    let org = match email::address_token(recipient, &inbound.domain) {
        Some(token) => match state.store.org_for_inbound_token(&token).await {
            Ok(org) => org,
            Err(e) => {
                tracing::error!("inbound token lookup: {e:#}");
                return reply(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unavailable",
                    "try again later",
                );
            }
        },
        None => None,
    };
    let Some(org) = org else {
        // Not an address we issued, or one since replaced. Nothing to retry.
        tracing::info!("inbound email to unknown address {recipient}");
        return reply(
            StatusCode::OK,
            "ignored",
            "no organization has this address",
        );
    };

    let actor = Actor::agent(org, "email");
    let input = json!({
        "raw_base64": base64::engine::general_purpose::STANDARD.encode(&body),
        "recipient": recipient,
    });
    match commands::run(
        &state,
        &actor,
        "email.import",
        input,
        Mode::Commit {
            confirm_token: None,
        },
    )
    .await
    {
        Ok(out) => (
            StatusCode::OK,
            Json(json!({ "status": "imported", "result": out["result"] })),
        )
            .into_response(),
        Err(CommandError::Conflict(msg)) => reply(StatusCode::OK, "duplicate", &msg),
        // The message itself is the problem; sending it again won't help.
        Err(CommandError::Invalid(msg)) => reply(StatusCode::OK, "rejected", &msg),
        Err(e) => {
            tracing::error!("inbound email for {org}: {e:#}");
            reply(
                StatusCode::SERVICE_UNAVAILABLE,
                e.code(),
                "couldn't import it; try again later",
            )
        }
    }
}

fn reply(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "status": code, "message": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same vector the worker's WebCrypto code produces, so the two
    /// sides can't drift apart.
    #[test]
    fn signature_matches_the_worker() {
        let inbound =
            Inbound::new("in.example.com", "test-inbound-secret-0123456789abcdef").unwrap();
        let body = b"From: a@b.c\r\n\r\nhi\r\n";
        let sig = inbound.sign(1_800_000_000, "k7f3q9x2m4p8@in.example.com", body);
        assert_eq!(
            sig,
            "bc1e6e894f121350fae389ad70e5b1aad70e8abb6a01a7f533d587339008b3b8"
        );
        assert!(inbound.verify(1_800_000_000, "k7f3q9x2m4p8@in.example.com", body, &sig));
        assert!(!inbound.verify(1_800_000_001, "k7f3q9x2m4p8@in.example.com", body, &sig));
        assert!(Inbound::new("in.example.com", "short").is_err());
    }
}
