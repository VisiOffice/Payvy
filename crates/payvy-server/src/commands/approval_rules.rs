//! Company-wide approval rules (separation of duties). Reading them is open
//! to agents, so they can tell people who must approve; changing them is a
//! person's act and lands in the event log. The rules themselves are applied
//! in `bills::approve` through `payvy_domain::duties`.

use payvy_store as store;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CommandError, Cx, text};

pub async fn get(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    Ok(json!(store::approval_rules(&mut cx.tx).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Set {
    submitter_cannot_approve: bool,
    reason: Option<String>,
}

pub async fn set(cx: &mut Cx<'_>, input: Set) -> Result<Value, CommandError> {
    let was = store::approval_rules(&mut cx.tx).await?;
    if was.submitter_cannot_approve == input.submitter_cannot_approve {
        return Err(CommandError::Conflict(format!(
            "submitter_cannot_approve is already {}",
            if input.submitter_cannot_approve {
                "on"
            } else {
                "off"
            }
        )));
    }
    store::set_approval_rules(
        &mut cx.tx,
        cx.org,
        input.submitter_cannot_approve,
        &cx.actor.name,
        cx.now,
    )
    .await?;
    let org = cx.org;
    cx.emit(
        "approval_rules.changed",
        org,
        json!({ "submitter_cannot_approve": input.submitter_cannot_approve,
                "was": was.submitter_cannot_approve, "reason": text(input.reason) }),
    )
    .await?;
    get(cx).await
}
