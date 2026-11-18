//! Agent API keys, made and revoked by an Admin in Settings → Agents. An
//! agent key prepares work (reads documents, enters bills, proposes payees)
//! and never signs off: person-only commands refuse it. The key is returned
//! once, by `agent_key.create`, and never stored or shown again; only its
//! hash is kept, and events name the key by id.

use payvy_store as store;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, people::valid_actor};

/// Names Payvy's own workers act under. An agent key can't take one, so
/// the record never mixes a customer's agent with Payvy's.
const RESERVED: [&str; 7] = [
    "bank",
    "email",
    "ledger",
    "notifications",
    "operator",
    "workflow",
    "payvy",
];

pub async fn list(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let keys = store::agent_keys(&mut cx.tx, cx.org).await?;
    Ok(json!({ "keys": keys }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    name: String,
}

pub async fn create(cx: &mut Cx<'_>, input: Create) -> Result<Value, CommandError> {
    let actor = input.name.trim().to_lowercase();
    if !valid_actor(&actor) {
        return Err(CommandError::Invalid(format!(
            "agent name {actor:?} must be a-z 0-9 . _ - (up to 64), starting with a letter or digit"
        )));
    }
    if RESERVED.contains(&actor.as_str()) {
        return Err(CommandError::Invalid(format!(
            "{actor} is a name Payvy's own workers use; choose another"
        )));
    }
    if store::people(&mut cx.tx, cx.org)
        .await?
        .iter()
        .any(|p| p.actor == actor)
    {
        return Err(CommandError::Conflict(format!(
            "{actor} is a person here; give the agent its own name so the record tells them apart"
        )));
    }
    let (key, hash) = crate::new_api_key()?;
    let id = Uuid::new_v4();
    store::insert_agent_key(&mut cx.tx, cx.org, id, &actor, &hash, cx.now).await?;
    cx.emit(
        "agent_key.created",
        id,
        json!({ "key_id": id, "agent": actor }),
    )
    .await?;
    Ok(json!({
        "id": id,
        "actor": actor,
        "created_at": cx.now,
        "key": key,
        "note": "Shown once: copy it into the agent's settings now. Payvy keeps only a hash and can't show it again.",
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revoke {
    id: Uuid,
}

pub async fn revoke(cx: &mut Cx<'_>, input: Revoke) -> Result<Value, CommandError> {
    let (key, changed) = store::revoke_agent_key(&mut cx.tx, cx.org, input.id, cx.now)
        .await?
        .ok_or(CommandError::NotFound("agent key"))?;
    if !changed {
        return Err(CommandError::Conflict(format!(
            "this key for {} was already revoked",
            key.actor
        )));
    }
    cx.emit(
        "agent_key.revoked",
        key.id,
        json!({ "key_id": key.id, "agent": key.actor }),
    )
    .await?;
    Ok(
        json!({ "key": key, "note": "The agent's next call with this key is refused. What it did stays on the record." }),
    )
}
