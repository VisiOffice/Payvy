//! Workflows are proposed (by an agent or a person) and accepted by a
//! person. Accepting supersedes the active one; bills keep the version they
//! arrived under.

use payvy_domain::workflow::Workflow;
use payvy_store::{self as store, WorkflowRow};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, text};

/// A workflow as callers see it: steps, a one-line reading of each, and a chart.
pub(super) fn view(workflow: &Workflow) -> Value {
    json!({
        "steps": workflow.steps,
        "describe": workflow.steps.iter().map(|s| s.describe()).collect::<Vec<_>>(),
        "mermaid": workflow.mermaid(),
    })
}

fn proposal_view(p: &WorkflowRow) -> Value {
    json!({
        "proposal_id": p.id,
        "vendor_id": p.vendor_id,
        "status": p.status,
        "reason": p.reason,
        "proposed_by": p.proposed_by,
        "proposed_at": p.created_at,
        "decided_by": p.decided_by,
        "workflow": view(&p.workflow),
    })
}

/// The workflow new bills for this vendor (or the organization default)
/// follow, where it comes from, and open proposals.
pub(super) async fn current(
    cx: &mut Cx<'_>,
    vendor_id: Option<Uuid>,
) -> Result<Value, CommandError> {
    let own = store::active_workflow(&mut cx.tx, vendor_id).await?;
    let (source, row) = match (own, vendor_id) {
        (Some(w), Some(_)) => ("vendor", Some(w)),
        (Some(w), None) => ("organization", Some(w)),
        (None, Some(_)) => match store::active_workflow(&mut cx.tx, None).await? {
            Some(w) => ("organization", Some(w)),
            None => ("builtin", None),
        },
        (None, None) => ("builtin", None),
    };
    let workflow = row
        .as_ref()
        .map(|r| r.workflow.clone())
        .unwrap_or_else(Workflow::builtin_default);
    let proposals = store::proposals(&mut cx.tx, vendor_id).await?;
    Ok(json!({
        "source": source,
        "workflow_id": row.as_ref().map(|r| r.id),
        "accepted_by": row.as_ref().and_then(|r| r.decided_by.clone()),
        "workflow": view(&workflow),
        "proposals": proposals.iter().map(proposal_view).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Get {
    vendor_id: Option<Uuid>,
}

pub async fn get(cx: &mut Cx<'_>, input: Get) -> Result<Value, CommandError> {
    if let Some(id) = input.vendor_id
        && store::get_vendor(&mut cx.tx, id).await?.is_none()
    {
        return Err(CommandError::NotFound("vendor"));
    }
    current(cx, input.vendor_id).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Propose {
    vendor_id: Option<Uuid>,
    workflow: Workflow,
    reason: Option<String>,
}

/// Named approvers who are people in this company but can't approve
/// bills (no Approver or Admin role): the workflow would strand every
/// bill that reaches their step, since bills pin their workflow version.
/// Actors who aren't people here are left alone — a person may be added
/// under that name later.
async fn approver_refusal(cx: &mut Cx<'_>, workflow: &Workflow) -> Result<(), CommandError> {
    use payvy_domain::permissions::Permission;
    use payvy_domain::workflow::StepKind;
    let mut roles_of = std::collections::HashMap::new();
    for p in store::people(&mut cx.tx, cx.org).await? {
        roles_of.insert(
            p.actor.clone(),
            store::actor_roles(&mut cx.tx, cx.org, &p.actor).await?,
        );
    }
    let mut refused = Vec::new();
    for step in &workflow.steps {
        if let StepKind::Approve { approvers, .. } = &step.kind {
            for name in approvers {
                if let Some(roles) = roles_of.get(name)
                    && !Permission::Approve.allows(roles)
                {
                    refused.push(format!("step {}: {name}", step.id));
                }
            }
        }
    }
    if refused.is_empty() {
        Ok(())
    } else {
        Err(CommandError::Conflict(format!(
            "{} can't approve bills: give them the Approver (or Admin) role first, or name someone else",
            refused.join(", ")
        )))
    }
}

pub async fn propose(cx: &mut Cx<'_>, input: Propose) -> Result<Value, CommandError> {
    input
        .workflow
        .validate()
        .map_err(|e| CommandError::Invalid(e.to_string()))?;
    approver_refusal(cx, &input.workflow).await?;
    if let Some(id) = input.vendor_id
        && store::get_vendor(&mut cx.tx, id).await?.is_none()
    {
        return Err(CommandError::NotFound("vendor"));
    }
    if let Some(active) = store::active_workflow(&mut cx.tx, input.vendor_id).await?
        && active.workflow == input.workflow
    {
        return Err(CommandError::Conflict(
            "that is already the active workflow".into(),
        ));
    }
    let reason = text(input.reason);
    let row = store::insert_proposal(
        &mut cx.tx,
        cx.org,
        input.vendor_id,
        &input.workflow,
        reason.as_deref(),
        &cx.actor.name,
    )
    .await?;
    cx.emit(
        "workflow.proposed",
        row.id,
        json!({ "proposal_id": row.id, "vendor_id": row.vendor_id, "reason": reason }),
    )
    .await?;
    let mut out = proposal_view(&row);
    out["next"] =
        json!("A person accepts it with workflow.accept, or declines it with workflow.decline.");
    Ok(out)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decide {
    proposal_id: Uuid,
    reason: Option<String>,
}

pub async fn decide(cx: &mut Cx<'_>, input: Decide, accept: bool) -> Result<Value, CommandError> {
    let row = store::get_workflow(&mut cx.tx, input.proposal_id)
        .await?
        .ok_or(CommandError::NotFound("proposal"))?;
    if row.status != "proposed" {
        return Err(CommandError::Conflict(format!(
            "this proposal is already {}",
            row.status
        )));
    }
    // Rechecked at acceptance: roles may have changed since it was proposed.
    if accept {
        approver_refusal(cx, &row.workflow).await?;
    }
    store::decide_proposal(&mut cx.tx, &row, accept, &cx.actor.name).await?;
    let kind = if accept {
        "workflow.accepted"
    } else {
        "workflow.declined"
    };
    cx.emit(
        kind,
        row.id,
        json!({ "proposal_id": row.id, "vendor_id": row.vendor_id, "reason": text(input.reason) }),
    )
    .await?;
    current(cx, row.vendor_id).await
}
