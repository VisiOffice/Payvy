//! Signing in to the web app with a link sent by email. No passwords: a
//! person asks for a link, the link opens the app, and the app trades it for
//! a session cookie. The person then acts exactly as a person API key would.
//!
//! - The link carries its token after `#`, so it never reaches a server log,
//!   and opening it does nothing until the app posts it: mail scanners that
//!   follow links can't use it up.
//! - Only SHA-256 hashes of link and session tokens are stored.
//! - Asking for a link answers the same whether or not the address can sign
//!   in, and sends the mail in the background so timing doesn't tell either.
//! - Cookie-authenticated API calls must carry the `x-payvy-client` header,
//!   which a cross-site form or link can't add.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use chrono::Duration;
use payvy_store::SignedIn;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::mailer::{Mail, Mailer};
use crate::{AppState, hash_key};

pub const COOKIE: &str = "payvy_session";
/// The header cookie-authenticated calls carry; any value.
pub const CLIENT_HEADER: &str = "x-payvy-client";
const LINK_MINUTES: i64 = 15;
const SESSION_DAYS: i64 = 30;
/// Links one person may be sent per window, so the endpoint can't be used to
/// flood someone's inbox.
const LINKS_PER_WINDOW: i64 = 5;

pub struct Auth {
    /// Where people open the app, e.g. https://beta.payvy.com. Links point here.
    pub public_url: String,
    pub mailer: Mailer,
}

impl Auth {
    pub fn new(public_url: &str, mailer: Mailer) -> Self {
        Self {
            public_url: public_url.trim_end_matches('/').to_owned(),
            mailer,
        }
    }

    fn secure(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/auth/link", post(send_link)).route(
        "/auth/session",
        post(start_session).get(current).delete(end_session),
    )
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

fn not_configured() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "not_configured",
        "Signing in to the web app is off: set PAYVY_PUBLIC_URL.",
    )
}

fn internal(e: anyhow::Error) -> Response {
    tracing::error!("auth: {e:#}");
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "Something went wrong.",
    )
}

pub fn random_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(hex::encode(bytes))
}

#[derive(Deserialize)]
struct LinkRequest {
    email: String,
}

async fn send_link(State(state): State<AppState>, Json(body): Json<LinkRequest>) -> Response {
    let Some(auth) = state.auth.clone() else {
        return not_configured();
    };
    let email = body.email.trim().to_lowercase();
    if !email.contains('@') || email.len() > 320 {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid",
            "That doesn't look like an email address.",
        );
    }
    let now = (state.clock)();
    let found = match state.store.people_by_email(&email).await {
        Ok(f) => f,
        Err(e) => return internal(e),
    };
    let mut links = Vec::new();
    for s in found {
        match state
            .store
            .recent_sign_in_links(s.person.id, now - Duration::minutes(LINK_MINUTES))
            .await
        {
            Ok(n) if n >= LINKS_PER_WINDOW => {
                tracing::warn!(person = %s.person.id, "sign-in links: limit reached");
                continue;
            }
            Ok(_) => {}
            Err(e) => return internal(e),
        }
        let token = match random_token() {
            Ok(t) => t,
            Err(e) => return internal(e),
        };
        if let Err(e) = state
            .store
            .insert_sign_in_link(
                &hash_key(&token),
                s.person.id,
                now,
                now + Duration::minutes(LINK_MINUTES),
            )
            .await
        {
            return internal(e);
        }
        links.push((
            s.org_name,
            format!("{}/sign-in#token={token}", auth.public_url),
        ));
    }
    if !links.is_empty() {
        let mail = sign_in_mail(&email, &links);
        // In the background, so the answer takes as long either way.
        tokio::spawn(async move {
            if let Err(e) = auth.mailer.send(mail).await {
                tracing::error!("sign-in mail not sent: {e:#}");
            }
        });
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "message": "If that address can sign in to Payvy, a link is on its way. It works once, for 15 minutes."
        })),
    )
        .into_response()
}

fn sign_in_mail(to: &str, links: &[(String, String)]) -> Mail {
    let body = if let [(_, link)] = links {
        format!("Open this link to sign in to Payvy:\n\n{link}\n")
    } else {
        let list: Vec<String> = links
            .iter()
            .map(|(org, link)| format!("{org}:\n{link}\n"))
            .collect();
        format!(
            "You can sign in to more than one company. Open the link for the one you want:\n\n{}",
            list.join("\n")
        )
    };
    Mail {
        to: to.to_owned(),
        subject: "Sign in to Payvy".into(),
        text: format!(
            "{body}\nThe link works once, for {LINK_MINUTES} minutes. If you didn't ask for it, ignore this email: nobody can sign in without opening it.\n"
        ),
    }
}

fn me(s: &SignedIn) -> Value {
    json!({
        "person": { "actor": s.person.actor, "name": s.person.name, "email": s.person.email },
        "organization": { "id": s.person.org_id, "name": s.org_name },
    })
}

#[derive(Deserialize)]
struct SessionRequest {
    token: String,
}

async fn start_session(
    State(state): State<AppState>,
    Json(body): Json<SessionRequest>,
) -> Response {
    let Some(auth) = state.auth.clone() else {
        return not_configured();
    };
    let now = (state.clock)();
    let session = match random_token() {
        Ok(t) => t,
        Err(e) => return internal(e),
    };
    let signed_in = match state
        .store
        .redeem_sign_in_link(
            &hash_key(body.token.trim()),
            &hash_key(&session),
            now,
            now + Duration::days(SESSION_DAYS),
        )
        .await
    {
        Ok(Some(s)) => s,
        Ok(None) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "link_expired",
                "This link has expired or was already used. Ask for a new one.",
            );
        }
        Err(e) => return internal(e),
    };
    let max_age = Duration::days(SESSION_DAYS).num_seconds();
    let cookie = format!(
        "{COOKIE}={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if auth.secure() { "; Secure" } else { "" }
    );
    let mut res = Json(me(&signed_in)).into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("cookie is ASCII"),
    );
    res
}

/// The session token from the request's cookies, if any.
pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_owned())
}

/// Who this cookie signs in, if anyone.
pub async fn signed_in(state: &AppState, headers: &HeaderMap) -> anyhow::Result<Option<SignedIn>> {
    let Some(token) = session_cookie(headers) else {
        return Ok(None);
    };
    state
        .store
        .session(&hash_key(&token), (state.clock)())
        .await
}

async fn current(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match signed_in(&state, &headers).await {
        Ok(Some(s)) => Json(me(&s)).into_response(),
        Ok(None) => error(StatusCode::UNAUTHORIZED, "signed_out", "Not signed in."),
        Err(e) => internal(e),
    }
}

async fn end_session(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = session_cookie(&headers)
        && let Err(e) = state
            .store
            .end_session(&hash_key(&token), (state.clock)())
            .await
    {
        return internal(e);
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("payvy_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    res
}
