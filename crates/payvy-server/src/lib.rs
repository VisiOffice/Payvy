pub mod auth;
pub mod bill_batch_worker;
pub mod commands;
pub mod confirm;
pub mod entitlements;
pub mod eval;
pub mod extract_worker;
pub mod inbound;
pub mod ledger_worker;
pub mod mailer;
pub mod mcp;
pub mod mercury_webhook;
pub mod notification_worker;
pub mod operator;
pub mod page_count;
pub mod payment_worker;
pub mod secrets;
pub mod sigv4;
pub mod source_text;

mod permissions;

use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use payvy_domain::email;
use payvy_store::{ActorKind, Store};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::commands::{CommandError, Mode};
use crate::confirm::Confirmer;

/// Who is calling: every command and event records this. People can sign
/// off (approve, release, accept a workflow); agents can do everything else.
#[derive(Debug, Clone)]
pub struct Actor {
    pub org_id: Uuid,
    pub name: String,
    pub kind: ActorKind,
    bootstrap: bool,
    /// Set when this caller authenticated with an `api_keys` row.
    /// The bootstrap admin key and a session cookie have none.
    pub key_id: Option<Uuid>,
}

impl Actor {
    /// Trusted server/CLI operator only. Never derive this from an actor name.
    pub fn administrator(org_id: Uuid, name: impl Into<String>) -> Self {
        Self {
            org_id,
            name: name.into(),
            kind: ActorKind::Person,
            bootstrap: true,
            key_id: None,
        }
    }

    pub fn person(org_id: Uuid, name: impl Into<String>) -> Self {
        Self {
            org_id,
            name: name.into(),
            kind: ActorKind::Person,
            bootstrap: false,
            key_id: None,
        }
    }

    pub fn agent(org_id: Uuid, name: impl Into<String>) -> Self {
        Self {
            org_id,
            name: name.into(),
            kind: ActorKind::Agent,
            bootstrap: false,
            key_id: None,
        }
    }

    /// The `api_keys` row this caller authenticated as.
    pub fn with_key(mut self, key_id: Uuid) -> Self {
        self.key_id = Some(key_id);
        self
    }
}

/// Where "now" comes from. The only place the server reads the clock, so
/// tests can move time to see a hold finish.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    /// The bootstrap key from PAYVY_API_KEY. It authenticates as the person
    /// `admin` in the default organization and signs confirm tokens. Keys
    /// for named people and agents live in `api_keys`.
    pub admin_key: Arc<str>,
    /// Cross-company service credential from PAYVY_OPERATOR_KEY. Never stored
    /// in `api_keys` and not tied to any company (`org_id`).
    pub operator_key: Option<Arc<str>>,
    /// Hosted Payvy (`PAYVY_HOSTED=true`): each company's plan entitlement
    /// gates the built-in reader and new payments (`entitlements.rs`).
    /// Self-hosted (false) has no plans: everything is on.
    pub hosted: bool,
    pub confirm: Arc<Confirmer>,
    pub clock: Clock,
    /// None when no object store is configured: document commands then say
    /// so instead of failing obscurely.
    pub storage: Option<Arc<payvy_storage::Storage>>,
    /// None when email intake isn't configured: the webhook answers 404 and
    /// the email commands say so.
    pub inbound: Option<Arc<inbound::Inbound>>,
    /// None when no model is configured to read documents: document.extract
    /// says so, and the background reader doesn't run.
    pub extractor: Option<Arc<payvy_extract::Extractor>>,
    /// None when PAYVY_SECRETS_KEY isn't set: bank commands say so, and
    /// bills are paid outside Payvy and marked paid.
    pub bank: Option<Arc<Bank>>,
    /// None when PAYVY_PUBLIC_URL isn't set: nobody can sign in to the web
    /// app, and API keys still work.
    pub auth: Option<Arc<auth::Auth>>,
    /// Woken when someone asks for bills from a file of several invoices,
    /// so the batch worker starts at once instead of on its next pass.
    pub bill_batches: Arc<tokio::sync::Notify>,
    /// The built web app (web/dist), served for every path the API doesn't
    /// claim. None serves only the API.
    pub web_dir: Option<std::path::PathBuf>,
    /// None until a ledger adapter is configured. Posting then says so.
    /// Tests pass a fake. Production does not talk to a ledger yet.
    pub ledger: Option<Arc<dyn payvy_ledger::Ledger + Send + Sync>>,
}

/// Paying through the customer's bank: the key bank tokens are sealed with,
/// and where each bank's API is (tests point these at a fake).
pub struct Bank {
    pub sealer: secrets::Sealer,
    pub ramp: Option<ramp::Config>,
    pub mercury_production: String,
    pub mercury_sandbox: String,
    pub rho_production: String,
    pub rho_sandbox: String,
    /// Where Mercury can reach this server (PAYVY_WEBHOOK_URL, e.g.
    /// https://dev.payvy.com). With it, connecting Mercury registers a
    /// webhook so payment updates arrive at once; without it the payment
    /// checker's polling is the only path.
    pub webhook_base: Option<String>,
}

impl Bank {
    pub fn new(sealer: secrets::Sealer) -> Self {
        Self {
            sealer,
            ramp: None,
            mercury_production: payvy_pay::MERCURY_PRODUCTION.into(),
            mercury_sandbox: payvy_pay::MERCURY_SANDBOX.into(),
            rho_production: payvy_pay::RHO_PRODUCTION.into(),
            rho_sandbox: payvy_pay::RHO_SANDBOX.into(),
            webhook_base: None,
        }
    }
}

impl AppState {
    pub async fn connect(database_url: &str, admin_key: &str) -> anyhow::Result<Self> {
        Ok(Self::new(Store::connect(database_url).await?, admin_key))
    }

    pub fn new(store: Store, admin_key: &str) -> Self {
        Self {
            store,
            admin_key: admin_key.into(),
            operator_key: None,
            hosted: false,
            confirm: Arc::new(Confirmer::new(admin_key.as_bytes())),
            clock: Arc::new(Utc::now),
            storage: None,
            inbound: None,
            extractor: None,
            bank: None,
            auth: None,
            bill_batches: Arc::new(tokio::sync::Notify::new()),
            web_dir: None,
            ledger: None,
        }
    }

    pub fn with_storage(mut self, storage: payvy_storage::Storage) -> Self {
        self.storage = Some(Arc::new(storage));
        self
    }

    pub fn with_inbound(mut self, inbound: inbound::Inbound) -> Self {
        self.inbound = Some(Arc::new(inbound));
        self
    }

    pub fn with_extractor(mut self, extractor: payvy_extract::Extractor) -> Self {
        self.extractor = Some(Arc::new(extractor));
        self
    }

    pub fn with_bank(mut self, bank: Bank) -> Self {
        self.bank = Some(Arc::new(bank));
        self
    }

    pub fn with_auth(mut self, auth: auth::Auth) -> Self {
        self.auth = Some(Arc::new(auth));
        self
    }

    pub fn with_web(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.web_dir = Some(dir.into());
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_ledger(mut self, ledger: impl payvy_ledger::Ledger + 'static) -> Self {
        self.ledger = Some(Arc::new(ledger));
        self
    }

    pub fn with_operator_key(mut self, key: &str) -> Self {
        assert!(
            key.len() >= 32,
            "operator_key must be at least 32 characters"
        );
        assert!(
            !secret_eq(key, &self.admin_key),
            "operator_key must differ from admin_key"
        );
        self.operator_key = Some(key.into());
        self
    }

    /// Hosted mode: plan entitlements are enforced. Requires the operator
    /// key, since only the operator can grant one.
    pub fn hosted(mut self) -> Self {
        assert!(
            self.operator_key.is_some(),
            "hosted mode needs the operator key (PAYVY_OPERATOR_KEY)"
        );
        self.hosted = true;
        self
    }

    pub(crate) fn is_admin_key(&self, given: &str) -> bool {
        !self.admin_key.is_empty() && secret_eq(given, &self.admin_key)
    }

    pub(crate) fn is_operator_key(&self, given: &str) -> bool {
        self.operator_key
            .as_deref()
            .is_some_and(|op| !op.is_empty() && secret_eq(given, op))
    }
}

/// A new random API key and the hash that gets stored. The key is shown once.
pub fn new_api_key() -> anyhow::Result<(String, String)> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    let key = format!("pv_{}", hex::encode(bytes));
    let hash = hash_key(&key);
    Ok((key, hash))
}

pub fn hash_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub fn router(state: AppState) -> Router {
    let authed = Router::new()
        .route("/api/commands", get(list_commands))
        .route("/api/commands/{name}", post(run_command))
        .route("/api/documents/{id}/file", get(document_file))
        .route("/api/documents/{id}/cover", get(document_cover))
        .route("/mcp", post(mcp::handle))
        // A 25 MB document arrives as about 34 MB of base64 in JSON.
        .layer(axum::extract::DefaultBodyLimit::max(36 * 1024 * 1024))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    // Signed by the mail front end instead of an API key; see `inbound`.
    let webhooks = Router::new()
        .route("/webhooks/email", post(inbound::webhook))
        .route(
            "/webhooks/mercury/{org}/{connection}",
            post(mercury_webhook::webhook),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            email::MAX_RAW_BYTES + 1024,
        ));
    let web_dir = state.web_dir.clone();
    let operator_routes = operator::routes(state.clone());
    let api = Router::new()
        .route("/health", get(health))
        .merge(authed)
        .merge(operator_routes)
        .merge(webhooks)
        .merge(auth::routes())
        .with_state(state);
    match web_dir {
        // The app routes in the browser, so any other path gets index.html.
        Some(dir) => api.fallback_service(
            tower_http::services::ServeDir::new(&dir)
                .fallback(tower_http::services::ServeFile::new(dir.join("index.html"))),
        ),
        None => api,
    }
}

async fn health(State(state): State<AppState>) -> Response {
    match state.store.ping().await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => {
            tracing::error!("health: {e:#}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "ok": false })),
            )
                .into_response()
        }
    }
}

async fn authenticate(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let given = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    if let Some(ref k) = given
        && state.is_operator_key(k)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": {
                    "code": "forbidden",
                    "message": "operator credential cannot be used on company endpoints"
                }
            })),
        )
            .into_response();
    }
    let actor = match given {
        Some(k) if state.is_admin_key(&k) => {
            Some(Actor::administrator(payvy_store::DEFAULT_ORG, "admin"))
        }
        Some(k) => match state.store.actor_for_key_hash(&hash_key(&k)).await {
            Ok(found) => found.map(|(org_id, key_id, name, kind)| Actor {
                org_id,
                name,
                kind,
                bootstrap: false,
                key_id: Some(key_id),
            }),
            Err(e) => {
                tracing::error!("api key lookup: {e:#}");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        },
        // The web app: a session cookie, on a call that carries the client
        // header (a cross-site form can't add one).
        None if req.headers().contains_key(auth::CLIENT_HEADER) => {
            match auth::signed_in(&state, req.headers()).await {
                Ok(s) => s.map(|s| Actor::person(s.person.org_id, s.person.actor)),
                Err(e) => {
                    tracing::error!("session lookup: {e:#}");
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
            }
        }
        None => None,
    };
    match actor {
        Some(actor) => {
            if actor.kind == ActorKind::Person {
                match permissions::visible(&state, &actor).await {
                    Ok(commands) if !commands.is_empty() => {},
                    Ok(_) => return (StatusCode::FORBIDDEN, Json(json!({ "error": { "code": "forbidden", "message": "No company roles are assigned to this person" } }))).into_response(),
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
            }
            req.extensions_mut().insert(actor);
            next.run(req).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "code": "unauthorized", "message": "missing or wrong API key" } })),
        )
            .into_response(),
    }
}

/// Constant-time comparison over fixed-size SHA-256 digests so neither secret
/// contents nor secret lengths leak via timing.
pub fn secret_eq(a: &str, b: &str) -> bool {
    let ha = Sha256::digest(a.as_bytes());
    let hb = Sha256::digest(b.as_bytes());
    ha.iter()
        .zip(hb.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// The commands this caller may run: an agent doesn't see person-only ones.
async fn list_commands(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<Value>, StatusCode> {
    let visible = permissions::visible(&state, &actor)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "commands": visible })))
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModeName {
    Preview,
    Commit,
}

#[derive(Deserialize)]
struct RunBody {
    #[serde(default = "empty_object")]
    input: Value,
    #[serde(default = "default_mode")]
    mode: ModeName,
    confirm_token: Option<String>,
}

fn empty_object() -> Value {
    json!({})
}

fn default_mode() -> ModeName {
    ModeName::Commit
}

async fn run_command(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(name): Path<String>,
    Json(body): Json<RunBody>,
) -> Result<Json<Value>, CommandError> {
    let mode = match body.mode {
        ModeName::Preview => Mode::Preview,
        ModeName::Commit => Mode::Commit {
            confirm_token: body.confirm_token,
        },
    };
    commands::run(&state, &actor, &name, body.input, mode)
        .await
        .map(Json)
}

/// A document's file, from storage, for the web app's viewer: same-origin,
/// so the storage bucket needs no CORS. Only PDFs and images are stored
/// (`documents::check`); they're served inline with no sniffing, and a
/// sandbox in case a browser renders one as a page.
async fn document_file(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Response, CommandError> {
    let storage = state
        .storage
        .as_deref()
        .ok_or(CommandError::NotConfigured("document storage"))?;
    let mut tx = state.store.begin(actor.org_id).await?;
    let doc = payvy_store::get_document(&mut tx, id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    tx.rollback().await.map_err(anyhow::Error::from)?;
    let bytes = storage.get(&doc.storage_key).await?;
    Ok((
        [
            (header::CONTENT_TYPE, doc.content_type),
            (header::CONTENT_DISPOSITION, "inline".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::CONTENT_SECURITY_POLICY, "sandbox".to_owned()),
            (header::CACHE_CONTROL, "private, max-age=3600".to_owned()),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
struct CoverQuery {
    page: Option<u32>,
}

/// A small image of one page (the first by default), for document lists.
/// An image document is its own cover.
async fn document_cover(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<CoverQuery>,
) -> Result<Response, CommandError> {
    let storage = state
        .storage
        .as_deref()
        .ok_or(CommandError::NotConfigured("document storage"))?;
    let mut tx = state.store.begin(actor.org_id).await?;
    let doc = payvy_store::get_document(&mut tx, id)
        .await?
        .ok_or(CommandError::NotFound("document"))?;
    tx.rollback().await.map_err(anyhow::Error::from)?;
    let bytes = storage.get(&doc.storage_key).await?;
    let (content_type, body) = if doc.content_type == "application/pdf" {
        let page = q.page.unwrap_or(1).max(1);
        let png = source_text::cover(&bytes, page)
            .await
            .ok_or(CommandError::NotFound("page"))?;
        ("image/png".to_owned(), png)
    } else {
        (doc.content_type, bytes.to_vec())
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::CONTENT_SECURITY_POLICY, "sandbox".to_owned()),
            // The file never changes; the page asked for is in the URL.
            (header::CACHE_CONTROL, "private, max-age=86400".to_owned()),
        ],
        body,
    )
        .into_response())
}

impl IntoResponse for CommandError {
    fn into_response(self) -> Response {
        let message = match &self {
            CommandError::Internal(e) => {
                tracing::error!("{e:#}");
                "internal error".to_owned()
            }
            other => other.to_string(),
        };
        (
            self.status(),
            Json(json!({ "error": { "code": self.code(), "message": message } })),
        )
            .into_response()
    }
}

pub mod ramp;

pub mod ramp_worker;
