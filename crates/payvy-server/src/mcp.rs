//! MCP over streamable HTTP (JSON responses only). Tools are generated from
//! the command registry, so a new command is an agent tool the moment it
//! exists. Confirmed-tier tools preview when called without `confirm_token`
//! and commit when called with the token that preview returned. An agent's
//! key doesn't list person-only tools.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use payvy_domain::commands::{CommandSpec, RiskTier};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::commands::{self, Mode};
use crate::{Actor, AppState};

const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Deserialize)]
pub struct Request {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

// MCP tool names allow [A-Za-z0-9_-]; commands use dots.
fn tool_name(spec: &CommandSpec) -> String {
    spec.name.replace('.', "_")
}

fn tool(spec: &CommandSpec) -> Value {
    let mut schema: Value =
        serde_json::from_str(spec.input_schema).expect("registry schemas are tested");
    let mut description = spec.summary.to_owned();
    if spec.tier == RiskTier::Confirmed {
        schema["properties"]["confirm_token"] = json!({
            "type": "string",
            "description": "Omit to preview. Pass the token from the preview to commit."
        });
        description.push_str(" Without confirm_token this only previews.");
    }
    json!({
        "name": tool_name(spec),
        "description": description,
        "inputSchema": schema,
        "annotations": { "readOnlyHint": spec.tier == RiskTier::Read },
    })
}

pub async fn handle(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(req): Json<Request>,
) -> Response {
    // Notifications carry no id and get no response body.
    let Some(id) = req.id else {
        return StatusCode::ACCEPTED.into_response();
    };
    let result = match req.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "payvy", "version": env!("CARGO_PKG_VERSION") },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => match crate::permissions::visible(&state, &actor).await {
            Ok(specs) => Ok(json!({ "tools": specs.into_iter().map(tool).collect::<Vec<_>>() })),
            Err(_) => Err(json!({ "code": -32603, "message": "Could not load permissions" })),
        },
        "tools/call" => Ok(call(&state, &actor, &req.params).await),
        other => Err(json!({ "code": -32601, "message": format!("method not found: {other}") })),
    };
    let body = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    };
    Json(body).into_response()
}

async fn call(state: &AppState, actor: &Actor, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let Ok(specs) = crate::permissions::visible(state, actor).await else {
        return tool_error("Could not load permissions");
    };
    let Some(spec) = specs.into_iter().find(|c| tool_name(c) == name) else {
        return tool_error(&format!("unknown tool {name}"));
    };
    let mut input = match params.get("arguments") {
        Some(Value::Object(m)) => m.clone(),
        None | Some(Value::Null) => Default::default(),
        Some(_) => return tool_error("arguments must be an object"),
    };
    // The token is not part of the input it signs.
    let token = input
        .remove("confirm_token")
        .and_then(|t| t.as_str().map(str::to_owned));
    let mode = match (spec.tier, token) {
        (RiskTier::Confirmed, None) => Mode::Preview,
        (_, confirm_token) => Mode::Commit { confirm_token },
    };
    match commands::run(state, actor, spec.name, Value::Object(input), mode).await {
        Ok(out) => {
            json!({ "content": [{ "type": "text", "text": out.to_string() }], "structuredContent": out })
        }
        Err(e) => {
            if let commands::CommandError::Internal(err) = &e {
                tracing::error!(command = spec.name, "{err:#}");
            }
            tool_error(&format!("{}: {e}", e.code()))
        }
    }
}

fn tool_error(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}
