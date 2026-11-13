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
//! - A link proves the email address, not a company: one link however many
//!   companies the address belongs to. Opening it signs in when there is one
//!   (or the link was sent for one, as after signup); with several, it
//!   returns a choice ticket and the companies, and the person picks one.
//!   Membership is checked when a session starts or switches company.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use chrono::Duration;
use payvy_store::{NotSignedIn, OpenedLink, SignedIn};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::mailer::{Mail, Mailer};
use crate::{AppState, hash_key};

pub const COOKIE: &str = "payvy_session";
/// The header cookie-authenticated calls carry; any value.
pub const CLIENT_HEADER: &str = "x-payvy-client";
const LINK_MINUTES: i64 = 15;
/// How long a person has to pick a company after opening a link.
const CHOICE_MINUTES: i64 = 10;
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
    Router::new()
        .route("/auth/link", post(send_link))
        .route(
            "/auth/session",
            post(start_session).get(current).delete(end_session),
        )
        .route("/auth/session/choose", post(choose))
        .route("/auth/session/switch", post(switch))
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
    return_to: Option<String>,
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
    // Only a bill route may survive sign-in; never accept an external redirect.
    let return_to = body
        .return_to
        .as_deref()
        .and_then(|p| p.strip_prefix("/bills/"))
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .map(|id| format!("?return_to=/bills/{id}"))
        .unwrap_or_default();
    if let Err(e) = email_link(&state, &auth, &email, None, &return_to).await {
        return internal(e);
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "message": "If that address can sign in to Payvy, a link is on its way. It works once, for 15 minutes."
        })),
    )
        .into_response()
}

/// Emails one sign-in link to an address that belongs to at least one
/// company, at most LINKS_PER_WINDOW per window. `org` opens it in that
/// company (signup, an invitation). Returns whether a link was sent; the
/// public endpoint answers the same either way. The mail goes out in the
/// background, so the answer takes as long either way.
pub(crate) async fn email_link(
    state: &AppState,
    auth: &std::sync::Arc<Auth>,
    email: &str,
    org: Option<uuid::Uuid>,
    query: &str,
) -> anyhow::Result<bool> {
    let now = (state.clock)();
    if state.store.companies_for_email(email).await?.is_empty() {
        return Ok(false);
    }
    let sent = state
        .store
        .recent_email_links(email, now - Duration::minutes(LINK_MINUTES))
        .await?;
    if sent >= LINKS_PER_WINDOW {
        tracing::warn!("sign-in links: limit reached for an address");
        return Ok(false);
    }
    let token = random_token()?;
    state
        .store
        .insert_email_link(
            &hash_key(&token),
            email,
            org,
            now,
            now + Duration::minutes(LINK_MINUTES),
        )
        .await?;
    let mail = sign_in_mail(
        email,
        &format!("{}/sign-in{query}#token={token}", auth.public_url),
    );
    let auth = auth.clone();
    tokio::spawn(async move {
        if let Err(e) = auth.mailer.send(mail).await {
            tracing::error!("sign-in mail not sent: {e:#}");
        }
    });
    Ok(true)
}

fn sign_in_mail(to: &str, link: &str) -> Mail {
    Mail {
        to: to.to_owned(),
        subject: "Sign in to Payvy".into(),
        text: format!(
            "Open this link to sign in to Payvy:\n\n{link}\n\nThe link works once, for {LINK_MINUTES} minutes. If you didn't ask for it, ignore this email: nobody can sign in without opening it.\n"
        ),
    }
}

async fn me(state: &AppState, s: &SignedIn) -> anyhow::Result<Value> {
    let actor = crate::Actor::person(s.person.org_id, &s.person.actor);
    let mut tx = state.store.begin(actor.org_id).await?;
    let roles = crate::permissions::roles(&mut tx, &actor).await?;
    let commands: Vec<_> = payvy_domain::commands::COMMANDS
        .iter()
        .filter(|c| crate::permissions::allowed(&actor, &roles, c))
        .map(|c| c.name)
        .collect();
    // Features the company turned on, so the app shows only what is in use.
    let purchase_orders = payvy_store::purchase_orders::company_settings(&mut tx)
        .await?
        .enabled;
    // What the company's plan includes (hosted), so the app can say so.
    let stored = if state.hosted {
        payvy_store::entitlement(&mut tx, actor.org_id).await?
    } else {
        None
    };
    tx.rollback().await?;
    // The other companies this address can switch to.
    let companies = state.store.companies_for_email(&s.person.email).await?;
    Ok(json!({
        "roles": roles, "commands": commands,
        "person": { "actor": s.person.actor, "name": s.person.name, "email": s.person.email },
        "organization": { "id": s.person.org_id, "name": s.org_name },
        "companies": companies,
        "features": { "purchase_orders": purchase_orders },
        "plan": crate::entitlements::view(state.hosted, stored.as_ref()),
    }))
}

/// The signed-in answer: who, and the cookie for the new session.
async fn signed_in_response(
    state: &AppState,
    auth: &Auth,
    session: &str,
    signed_in: &SignedIn,
) -> Response {
    let max_age = Duration::days(SESSION_DAYS).num_seconds();
    let cookie = format!(
        "{COOKIE}={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if auth.secure() { "; Secure" } else { "" }
    );
    let body = match me(state, signed_in).await {
        Ok(body) => body,
        Err(e) => return internal(e),
    };
    let mut res = Json(body).into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("cookie is ASCII"),
    );
    res
}

fn link_expired() -> Response {
    error(
        StatusCode::UNAUTHORIZED,
        "link_expired",
        "This link has expired or was already used. Ask for a new one.",
    )
}

#[derive(Deserialize)]
struct SessionRequest {
    token: String,
}

/// Opens an emailed link. Signed in (a cookie and who), or, for an address
/// with several companies, `{ "choose": [...], "ticket": ... }` to pick one
/// with `/auth/session/choose`.
async fn start_session(
    State(state): State<AppState>,
    Json(body): Json<SessionRequest>,
) -> Response {
    let Some(auth) = state.auth.clone() else {
        return not_configured();
    };
    let now = (state.clock)();
    let (session, ticket) = match (random_token(), random_token()) {
        (Ok(s), Ok(t)) => (s, t),
        (Err(e), _) | (_, Err(e)) => return internal(e),
    };
    let link_hash = hash_key(body.token.trim());
    let opened = state
        .store
        .open_email_link(&payvy_store::OpenLink {
            link_hash: &link_hash,
            session_hash: &hash_key(&session),
            choice_hash: &hash_key(&ticket),
            at: now,
            session_expires_at: now + Duration::days(SESSION_DAYS),
            choice_expires_at: now + Duration::minutes(CHOICE_MINUTES),
        })
        .await;
    match opened {
        Ok(OpenedLink::SignedIn(s)) => signed_in_response(&state, &auth, &session, &s).await,
        Ok(OpenedLink::Choose(companies)) => Json(json!({
            "choose": companies,
            "ticket": ticket,
            "expires_in_minutes": CHOICE_MINUTES,
        }))
        .into_response(),
        // A link sent per person before links proved addresses (in flight
        // across the deploy that changed them).
        Ok(OpenedLink::Invalid) => match state
            .store
            .redeem_sign_in_link(
                &link_hash,
                &hash_key(&session),
                now,
                now + Duration::days(SESSION_DAYS),
            )
            .await
        {
            Ok(Some(s)) => signed_in_response(&state, &auth, &session, &s).await,
            Ok(None) => link_expired(),
            Err(e) => internal(e),
        },
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct ChooseRequest {
    ticket: String,
    org_id: uuid::Uuid,
}

/// Picks a company after opening a link, with the ticket that returned.
async fn choose(State(state): State<AppState>, Json(body): Json<ChooseRequest>) -> Response {
    let Some(auth) = state.auth.clone() else {
        return not_configured();
    };
    let now = (state.clock)();
    let session = match random_token() {
        Ok(t) => t,
        Err(e) => return internal(e),
    };
    match state
        .store
        .choose_company(
            &hash_key(body.ticket.trim()),
            body.org_id,
            &hash_key(&session),
            now,
            now + Duration::days(SESSION_DAYS),
        )
        .await
    {
        Ok(Ok(s)) => signed_in_response(&state, &auth, &session, &s).await,
        Ok(Err(NotSignedIn::Expired)) => error(
            StatusCode::UNAUTHORIZED,
            "link_expired",
            "Choosing took too long or was already done. Ask for a new link.",
        ),
        Ok(Err(NotSignedIn::NotMember)) => not_member(),
        Err(e) => internal(e),
    }
}

fn not_member() -> Response {
    error(
        StatusCode::FORBIDDEN,
        "not_member",
        "You can't sign in to that company. Ask one of its admins to add you.",
    )
}

#[derive(Deserialize)]
struct SwitchRequest {
    org_id: uuid::Uuid,
}

/// Moves this browser to another company the signed-in address belongs to.
/// Like every cookie-authenticated write, it needs the `x-payvy-client`
/// header, so a cross-site form can't switch someone's company.
async fn switch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SwitchRequest>,
) -> Response {
    let Some(auth) = state.auth.clone() else {
        return not_configured();
    };
    if !headers.contains_key(CLIENT_HEADER) {
        return error(
            StatusCode::FORBIDDEN,
            "client_header",
            "Switching companies needs the x-payvy-client header.",
        );
    }
    let Some(current) = session_cookie(&headers) else {
        return error(StatusCode::UNAUTHORIZED, "signed_out", "Not signed in.");
    };
    let now = (state.clock)();
    let session = match random_token() {
        Ok(t) => t,
        Err(e) => return internal(e),
    };
    match state
        .store
        .switch_company(
            &hash_key(&current),
            body.org_id,
            &hash_key(&session),
            now,
            now + Duration::days(SESSION_DAYS),
        )
        .await
    {
        Ok(Ok(s)) => signed_in_response(&state, &auth, &session, &s).await,
        Ok(Err(NotSignedIn::Expired)) => {
            error(StatusCode::UNAUTHORIZED, "signed_out", "Not signed in.")
        }
        Ok(Err(NotSignedIn::NotMember)) => not_member(),
        Err(e) => internal(e),
    }
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
        Ok(Some(s)) => match me(&state, &s).await {
            Ok(body) => Json(body).into_response(),
            Err(e) => internal(e),
        },
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
