pub mod auth;
pub mod commands;
pub mod confirm;
pub mod eval;
pub mod extract_worker;
pub mod inbound;
pub mod mailer;
pub mod mcp;
pub mod payment_worker;
pub mod secrets;
pub mod sigv4;

use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use payvy_domain::commands::COMMANDS;
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
}

impl Actor {
    pub fn person(org_id: Uuid, name: impl Into<String>) -> Self {
        Self {
            org_id,
            name: name.into(),
            kind: ActorKind::Person,
        }
    }

    pub fn agent(org_id: Uuid, name: impl Into<String>) -> Self {
        Self {
            org_id,
            name: name.into(),
            kind: ActorKind::Agent,
        }
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
    /// The built web app (web/dist), served for every path the API doesn't
    /// claim. None serves only the API.
    pub web_dir: Option<std::path::PathBuf>,
}

/// Paying through the customer's bank: the key bank tokens are sealed with,
/// and where each bank's API is (tests point these at a fake).
pub struct Bank {
    pub sealer: secrets::Sealer,
    pub mercury_production: String,
    pub mercury_sandbox: String,
}

impl Bank {
    pub fn new(sealer: secrets::Sealer) -> Self {
        Self {
            sealer,
            mercury_production: payvy_pay::MERCURY_PRODUCTION.into(),
            mercury_sandbox: payvy_pay::MERCURY_SANDBOX.into(),
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
            confirm: Arc::new(Confirmer::new(admin_key.as_bytes())),
            clock: Arc::new(Utc::now),
            storage: None,
            inbound: None,
            extractor: None,
            bank: None,
            auth: None,
            web_dir: None,
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
        .route("/mcp", post(mcp::handle))
        // A 25 MB document arrives as about 34 MB of base64 in JSON.
        .layer(axum::extract::DefaultBodyLimit::max(36 * 1024 * 1024))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    // Signed by the mail front end instead of an API key; see `inbound`.
    let webhooks = Router::new()
        .route("/webhooks/email", post(inbound::webhook))
        .layer(axum::extract::DefaultBodyLimit::max(
            email::MAX_RAW_BYTES + 1024,
        ));
    let web_dir = state.web_dir.clone();
    let api = Router::new()
        .route("/health", get(health))
        .merge(authed)
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
    let actor = match given {
        Some(k) if constant_time_eq(k.as_bytes(), state.admin_key.as_bytes()) => {
            Some(Actor::person(payvy_store::DEFAULT_ORG, "admin"))
        }
        Some(k) => match state.store.actor_for_key_hash(&hash_key(&k)).await {
            Ok(found) => found.map(|(org_id, name, kind)| Actor { org_id, name, kind }),
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

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The commands this caller may run: an agent doesn't see person-only ones.
async fn list_commands(Extension(actor): Extension<Actor>) -> Json<Value> {
    let visible: Vec<_> = COMMANDS
        .iter()
        .filter(|c| actor.kind == ActorKind::Person || !c.person_only)
        .collect();
    Json(json!({ "commands": visible }))
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
