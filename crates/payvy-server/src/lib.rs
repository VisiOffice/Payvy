pub mod commands;
pub mod confirm;
pub mod mcp;

use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use payvy_domain::commands::COMMANDS;
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
        }
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
        .route("/mcp", post(mcp::handle))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/health", get(health))
        .merge(authed)
        .with_state(state)
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
