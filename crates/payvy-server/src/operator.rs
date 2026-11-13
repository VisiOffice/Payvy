//! Operator API (`/operator/commands` and `/operator/commands/{name}`).
//!
//! Cross-company provisioning and usage reporting for the hosted control plane
//! and trusted CLI. Authenticated exclusively with `PAYVY_OPERATOR_KEY`, which
//! is never stored in `api_keys` and is not tied to any company (`org_id`).

use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use payvy_domain::ai_usage::{format_usd, utc_month_bounds};
use payvy_domain::commands;
use payvy_domain::workflow::DEFAULT_AUTO_PAY_LIMIT_CENTS;
use payvy_store::{AiUsageSummary, ProvisionOrgInput, ProvisionOrgOutcome};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::commands::{CommandError, Mode, valid_actor, valid_org_name};
use crate::{AppState, auth, hash_key};

const IDEMPOTENCY_KEY_MAX: usize = 255;
const PERSON_NAME_MAX: usize = 200;
const EMAIL_MAX: usize = 320;
const ORG_LIST_MAX: i64 = 500;

pub fn routes(state: AppState) -> Router<AppState> {
    use axum::routing::any;
    Router::new()
        .route("/operator/commands", get(list_commands))
        .route("/operator/commands/{name}", post(run_command))
        .route("/operator", any(not_found))
        .route("/operator/", any(not_found))
        .route("/operator/{*rest}", any(not_found))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .route_layer(middleware::from_fn_with_state(state, authenticate_operator))
}

async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": {
                "code": "not_found",
                "message": "not found"
            }
        })),
    )
        .into_response()
}

async fn authenticate_operator(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if state.operator_key.is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "code": "not_configured",
                    "message": "operator API is not configured"
                }
            })),
        )
            .into_response();
    }

    let given = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);

    if let Some(ref k) = given {
        if state.is_operator_key(k) {
            return next.run(req).await;
        }
        if state.is_admin_key(k) {
            return forbidden_company_credential();
        }
        match state.store.actor_for_key_hash(&hash_key(k)).await {
            Ok(Some(_)) => return forbidden_company_credential(),
            Ok(None) => {}
            Err(e) => {
                tracing::error!("operator auth key check: {e:#}");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
    } else {
        match auth::signed_in(&state, req.headers()).await {
            Ok(Some(_)) => return forbidden_company_credential(),
            Ok(None) => {}
            Err(e) => {
                tracing::error!("operator auth session check: {e:#}");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "code": "unauthorized",
                "message": "missing or wrong operator key"
            }
        })),
    )
        .into_response()
}

fn forbidden_company_credential() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "code": "forbidden",
                "message": "company credentials cannot access operator endpoints"
            }
        })),
    )
        .into_response()
}

async fn list_commands() -> Json<Value> {
    Json(json!({ "commands": commands::OPERATOR_COMMANDS }))
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
    Path(name): Path<String>,
    Json(body): Json<RunBody>,
) -> Result<Json<Value>, CommandError> {
    let mode = match body.mode {
        ModeName::Preview => Mode::Preview,
        ModeName::Commit => Mode::Commit {
            confirm_token: body.confirm_token,
        },
    };
    run(&state, &name, body.input, mode).await.map(Json)
}

/// Runs one operator command (`org.create`, `org.list`, `usage.summary`).
pub async fn run(
    state: &AppState,
    name: &str,
    input: Value,
    mode: Mode,
) -> Result<Value, CommandError> {
    let Some(spec) = commands::find_operator(name) else {
        if commands::find(name).is_some() {
            return Err(CommandError::Forbidden(format!(
                "{name} is a company command; operator credential is not tied to a company"
            )));
        }
        return Err(CommandError::Unknown(name.to_owned()));
    };

    let preview = matches!(mode, Mode::Preview);
    let (result, events) = match spec.name {
        "org.create" => create_org(state, parse(&input)?, preview).await?,
        "org.list" => (list_orgs(state, parse(&input)?).await?, Vec::new()),
        "usage.summary" => (usage_summary(state, parse(&input)?).await?, Vec::new()),
        other => return Err(CommandError::Unknown(other.to_owned())),
    };

    Ok(json!({
        "command": spec.name,
        "mode": if preview { "preview" } else { "commit" },
        "result": result,
        "events": events,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateOrg {
    idempotency_key: String,
    name: String,
    admin_email: String,
    admin_name: String,
    admin_actor: Option<String>,
}

async fn create_org(
    state: &AppState,
    input: CreateOrg,
    preview: bool,
) -> Result<(Value, Vec<String>), CommandError> {
    let idempotency_key = valid_idempotency_key(&input.idempotency_key)?;
    let org_name = valid_org_name(&input.name)?;
    let (admin_email, local) = valid_email(&input.admin_email)?;
    let admin_name = valid_person_name(&input.admin_name)?;
    let admin_actor = match input.admin_actor.map(|s| s.trim().to_owned()) {
        Some(a) if !a.is_empty() => {
            let lower = a.to_lowercase();
            if !valid_actor(&lower) {
                return Err(CommandError::Invalid(format!(
                    "admin_actor {lower:?} must be a-z 0-9 . _ - (up to 64), starting with a letter or digit"
                )));
            }
            lower
        }
        _ => derive_actor(&local),
    };

    let request_hash = hash_provision_request(&org_name, &admin_email, &admin_name, &admin_actor);
    let now = (state.clock)();
    let provisioned = match state
        .store
        .provision_organization(
            &ProvisionOrgInput {
                idempotency_key: &idempotency_key,
                request_hash: &request_hash,
                org_name: &org_name,
                admin_actor: &admin_actor,
                admin_email: &admin_email,
                admin_name: &admin_name,
                now,
            },
            preview,
        )
        .await?
    {
        ProvisionOrgOutcome::Ok(p) => *p,
        ProvisionOrgOutcome::IdempotencyConflict => {
            return Err(CommandError::Conflict(
                "idempotency_key was already used with different parameters".into(),
            ));
        }
    };

    let events = if provisioned.created {
        vec!["org.created".to_owned(), "person.added".to_owned()]
    } else {
        Vec::new()
    };
    let mut admin = json!(provisioned.admin);
    admin["roles"] = json!(provisioned.admin_roles);

    Ok((
        json!({
            "created": provisioned.created,
            "organization": provisioned.organization,
            "admin": admin,
            "defaults": {
                "workflow": "builtin",
                "auto_pay_limit_cents": DEFAULT_AUTO_PAY_LIMIT_CENTS,
                "purchase_orders_enabled": false,
                "notifications_enabled": false,
            },
        }),
        events,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListOrgs {
    limit: Option<i64>,
}

async fn list_orgs(state: &AppState, input: ListOrgs) -> Result<Value, CommandError> {
    let limit = input.limit.unwrap_or(ORG_LIST_MAX);
    if !(1..=ORG_LIST_MAX).contains(&limit) {
        return Err(CommandError::Invalid(format!(
            "limit must be between 1 and {ORG_LIST_MAX}"
        )));
    }
    let organizations = state.store.list_organizations(limit).await?;
    Ok(json!({ "organizations": organizations }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorUsageSummary {
    #[serde(default)]
    month: Option<String>,
    #[serde(default)]
    org_id: Option<Uuid>,
}

async fn usage_summary(
    state: &AppState,
    input: OperatorUsageSummary,
) -> Result<Value, CommandError> {
    let now = (state.clock)();
    let (month, start, end) =
        utc_month_bounds(now, input.month.as_deref()).map_err(CommandError::Invalid)?;
    let stale_before = now - chrono::Duration::seconds(payvy_extract::REQUEST_TIMEOUT_SECS);
    let (total, orgs) = state
        .store
        .operator_ai_usage_summary(input.org_id, start, end, stale_before)
        .await?
        .ok_or(CommandError::NotFound("organization"))?;

    let organizations: Vec<Value> = orgs
        .into_iter()
        .map(|o| {
            let mut v = summary_fields(&o.summary);
            v["org_id"] = json!(o.org_id);
            v["org_name"] = json!(o.org_name);
            v
        })
        .collect();

    let mut out = summary_fields(&total);
    out["month"] = json!(month);
    out["period_start"] = json!(start);
    out["period_end"] = json!(end);
    out["organizations"] = json!(organizations);
    Ok(out)
}

fn summary_fields(s: &AiUsageSummary) -> Value {
    json!({
        "attempts": s.attempts,
        "successful_reads": s.successful_reads,
        "failed_attempts": s.failed_attempts,
        "pending_attempts": s.pending_attempts,
        "unknown_usage_attempts": s.unknown_usage_attempts,
        "unpriced_attempts": s.unpriced_attempts,
        "input_tokens": s.input_tokens,
        "output_tokens": s.output_tokens,
        "cache_read_tokens": s.cache_read_tokens,
        "cache_write_tokens": s.cache_write_tokens,
        "estimated_cost_microcents": s.estimated_cost_microcents,
        "estimated_cost_usd": format_usd(s.estimated_cost_microcents),
        "currency": s.currency,
    })
}

fn valid_idempotency_key(raw: &str) -> Result<String, CommandError> {
    let k = raw.trim();
    if k.is_empty() {
        return Err(CommandError::Invalid(
            "idempotency_key can't be empty".into(),
        ));
    }
    if k.chars().count() > IDEMPOTENCY_KEY_MAX {
        return Err(CommandError::Invalid(format!(
            "idempotency_key can be at most {IDEMPOTENCY_KEY_MAX} characters"
        )));
    }
    if k.chars().any(char::is_control) {
        return Err(CommandError::Invalid(
            "idempotency_key can't contain control characters".into(),
        ));
    }
    Ok(k.to_owned())
}

fn valid_person_name(raw: &str) -> Result<String, CommandError> {
    let n = raw.trim();
    if n.is_empty() {
        return Err(CommandError::Invalid("admin_name can't be empty".into()));
    }
    if n.chars().count() > PERSON_NAME_MAX {
        return Err(CommandError::Invalid(format!(
            "admin_name can be at most {PERSON_NAME_MAX} characters"
        )));
    }
    if n.chars().any(char::is_control) {
        return Err(CommandError::Invalid(
            "admin_name can't contain line breaks or control characters".into(),
        ));
    }
    Ok(n.to_owned())
}

fn valid_email(raw: &str) -> Result<(String, String), CommandError> {
    let email = raw.trim().to_lowercase();
    if email.len() > EMAIL_MAX {
        return Err(CommandError::Invalid(format!(
            "admin_email can be at most {EMAIL_MAX} characters"
        )));
    }
    let Some((local, domain)) = email.split_once('@') else {
        return Err(CommandError::Invalid("admin_email needs an @".into()));
    };
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || email.contains(char::is_whitespace)
        || email.chars().any(char::is_control)
    {
        return Err(CommandError::Invalid(format!(
            "{email} isn't an email address"
        )));
    }
    Ok((email.clone(), local.to_owned()))
}

fn derive_actor(local: &str) -> String {
    let base = local.split('+').next().unwrap_or(local);
    if valid_actor(base) {
        return base.to_owned();
    }
    let cleaned: String = base
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(*c))
        .skip_while(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .take(64)
        .collect();
    if valid_actor(&cleaned) {
        cleaned
    } else {
        "admin".to_owned()
    }
}

fn hash_provision_request(
    org_name: &str,
    admin_email: &str,
    admin_name: &str,
    admin_actor: &str,
) -> String {
    let mut hasher = Sha256::new();
    for field in [org_name, admin_email, admin_name, admin_actor] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn parse<T: DeserializeOwned>(input: &Value) -> Result<T, CommandError> {
    serde_json::from_value(input.clone()).map_err(|e| CommandError::Invalid(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_clean_actor_handles_from_emails() {
        for (local, expected) in [
            ("a", "a"),
            ("7", "7"),
            ("a+tag", "a"),
            ("robert", "robert"),
            ("robert+billing", "robert"),
            ("_ops+1", "ops"),
            ("josé", "jos"),
            ("山田", "admin"),
            ("'o'neill", "oneill"),
            ("..--__", "admin"),
            ("+tag", "admin"),
            ("---", "admin"),
        ] {
            let got = derive_actor(local);
            assert_eq!(got, expected, "local={local:?}");
            assert!(valid_actor(&got), "invalid actor derived: {got:?}");
        }
        let long = derive_actor(&format!("---{}", "a".repeat(80)));
        assert_eq!(long.len(), 64);
        assert!(valid_actor(&long));
    }

    #[test]
    fn request_hash_is_collision_free_and_deterministic() {
        let a = hash_provision_request("Acme", "a@acme.com", "Alice", "alice");
        let b = hash_provision_request("Acme", "a@acme.com", "Alice", "alice");
        let c = hash_provision_request("Acme", "a@acme.com", "Alice", "bob");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
