//! What is waiting on a person: bills to approve or release, and payee and
//! workflow proposals to accept. One call for the web app's inbox and for an
//! agent asking what to nudge people about.

use std::collections::HashMap;

use payvy_store as store;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, bills, parse};

pub async fn get(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let listed = bills::list(cx, parse(&json!({ "mine": true, "limit": 500 }))?).await?;
    let bills: Vec<Value> = listed["bills"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut b| {
            let action = match b["progress"]["status"].as_str() {
                Some("awaiting_approval") => "approve",
                Some("awaiting_release") => "release",
                _ => "look",
            };
            b["action"] = json!(action);
            b
        })
        .collect();

    let mut names: HashMap<Uuid, String> = HashMap::new();
    let mut name_of = async |tx: &mut store::Tx, id: Uuid| -> Result<String, CommandError> {
        if let Some(n) = names.get(&id) {
            return Ok(n.clone());
        }
        let n = store::get_vendor(tx, id)
            .await?
            .map(|v| v.name)
            .unwrap_or_default();
        names.insert(id, n.clone());
        Ok(n)
    };

    let paying = store::live_paying_connection(&mut cx.tx).await?;
    let mut payees = Vec::new();
    for p in store::open_payee_proposals(&mut cx.tx).await? {
        let vendor_name = name_of(&mut cx.tx, p.vendor_id).await?;
        let current = store::active_payee(&mut cx.tx, p.vendor_id).await?;
        payees.push(json!({ "proposal": p, "vendor_name": vendor_name, "current": current,
            "is_current_connection":paying.as_ref().is_some_and(|c| c.id == p.connection_id),
            "current_is_current_connection":current.as_ref().is_some_and(|p| paying.as_ref().is_some_and(|c| c.id == p.connection_id)),
        }));
    }

    let mut workflows = Vec::new();
    for w in store::open_workflow_proposals(&mut cx.tx).await? {
        let vendor_name = match w.vendor_id {
            Some(id) => Some(name_of(&mut cx.tx, id).await?),
            None => None,
        };
        let current = store::active_workflow(&mut cx.tx, w.vendor_id).await?;
        workflows.push(json!({
            "proposal": w, "vendor_name": vendor_name,
            "current": current.map(|c| c.workflow),
        }));
    }

    Ok(json!({
        "bills": bills,
        "payee_changes": payees,
        "workflow_proposals": workflows,
        "count": bills_len(&listed) + payees.len() + workflows.len(),
    }))
}

fn bills_len(listed: &Value) -> usize {
    listed["bills"].as_array().map_or(0, Vec::len)
}
