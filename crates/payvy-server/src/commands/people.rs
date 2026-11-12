//! Who can sign in to the web app. A person signs in with a link sent to
//! their email and acts as the actor of the same name.

use payvy_store::{self as store, ActorKind, NewPerson, Role};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CommandError, Cx, text};

pub async fn list(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let mut people = Vec::new();
    for p in store::people(&mut cx.tx, cx.org).await? {
        let roles = store::actor_roles(&mut cx.tx, cx.org, &p.actor).await?;
        let mut row = json!(p);
        row["roles"] = json!(roles);
        people.push(row);
    }
    Ok(json!({ "people": people }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Add {
    email: String,
    name: String,
    actor: Option<String>,
    roles: Option<Vec<Role>>,
}

fn valid_actor(a: &str) -> bool {
    let mut chars = a.chars();
    a.len() <= 64
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

pub async fn add(cx: &mut Cx<'_>, input: Add) -> Result<Value, CommandError> {
    let roles = validate_roles(input.roles.unwrap_or_else(|| vec![Role::ApClerk]))?;
    let email = input.email.trim().to_lowercase();
    let Some((local, domain)) = email.split_once('@') else {
        return Err(CommandError::Invalid("email needs an @".into()));
    };
    if local.is_empty() || domain.is_empty() || email.contains(char::is_whitespace) {
        return Err(CommandError::Invalid(format!(
            "{email} isn't an email address"
        )));
    }
    let name = text(Some(input.name))
        .ok_or_else(|| CommandError::Invalid("name can't be empty".into()))?;
    let actor = match text(input.actor) {
        Some(a) => a.to_lowercase(),
        None => local.split('+').next().unwrap_or(local).to_owned(),
    };
    if !valid_actor(&actor) {
        return Err(CommandError::Invalid(format!(
            "actor {actor:?} must be a-z 0-9 . _ - (up to 64), starting with a letter or digit; pass actor to choose one"
        )));
    }
    let person = store::insert_person(
        &mut cx.tx,
        &NewPerson {
            org_id: cx.org,
            actor: &actor,
            email: &email,
            name: &name,
            created_at: cx.now,
            created_by: &cx.actor.name,
        },
    )
    .await?
    .ok_or_else(|| {
        CommandError::Conflict(format!(
            "someone here already signs in as {actor} or with {email}; pass a different actor, or person.remove them first"
        ))
    })?;
    store::set_actor_roles(&mut cx.tx, cx.org, &actor, &roles).await?;
    cx.emit(
        "person.added",
        person.id,
        json!({ "actor": person.actor, "email": person.email, "name": person.name, "roles": roles }),
    )
    .await?;
    let mut person = json!(person);
    person["roles"] = json!(roles);
    Ok(json!({
        "person": person,
        "next": "they open the web app and ask for a sign-in link with this email",
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    actor: String,
}

pub async fn remove(cx: &mut Cx<'_>, input: Remove) -> Result<Value, CommandError> {
    let actor = input.actor.trim().to_lowercase();
    let people = store::lock_people(&mut cx.tx, cx.org).await?;
    if !people.contains(&actor) {
        return Err(CommandError::NotFound("person"));
    }
    if cx.actor.kind == ActorKind::Person && cx.actor.name == actor {
        return Err(CommandError::Forbidden(
            "you can't remove yourself; ask someone else who signs in here".into(),
        ));
    }
    if people.len() <= 1 {
        return Err(CommandError::Conflict(format!(
            "{actor} is the last person who can sign in; add someone else first"
        )));
    }
    protect_last_admin(cx, &actor).await?;
    approver_strand_refusal(cx, &actor).await?;
    let id = store::remove_person(&mut cx.tx, cx.org, &actor, cx.now, &cx.actor.name)
        .await?
        .ok_or(CommandError::NotFound("person"))?;
    cx.emit("person.removed", id, json!({ "actor": actor }))
        .await?;
    Ok(
        json!({ "removed": actor, "note": "signed out everywhere; their past actions stay on the record" }),
    )
}

fn validate_roles(mut roles: Vec<Role>) -> Result<Vec<Role>, CommandError> {
    if roles.is_empty() || roles.len() > 4 {
        return Err(CommandError::Invalid(
            "choose at least one role (at most four)".into(),
        ));
    }
    roles.sort();
    let original = roles.len();
    roles.dedup();
    if roles.len() != original {
        return Err(CommandError::Invalid("a role is listed twice".into()));
    }
    Ok(roles)
}
/// Taking away the approval ability of someone an open bill is
/// currently waiting on would strand that bill: it keeps the workflow
/// version it arrived under, and nobody else can give that step. So
/// role loss (and removal) is refused exactly while a bill waits on
/// their approval step — and only then; role changes at other times
/// stay possible, and queued notices recheck eligibility before send.
async fn approver_strand_refusal(cx: &mut Cx<'_>, actor: &str) -> Result<(), CommandError> {
    use payvy_domain::bills::State;
    use payvy_domain::permissions::Permission;
    use payvy_domain::workflow::{self, Facts, Status, Workflow};
    let roles = store::actor_roles(&mut cx.tx, cx.org, actor).await?;
    if !Permission::Approve.allows(&roles) {
        return Ok(());
    }
    let bills = store::list_bills(&mut cx.tx, Some(State::Open), None, None, 5_000).await?;
    if bills.is_empty() {
        return Ok(());
    }
    let wf_ids: Vec<uuid::Uuid> = bills.iter().filter_map(|b| b.workflow_id).collect();
    let workflows: std::collections::HashMap<uuid::Uuid, Workflow> =
        store::workflows_by_id(&mut cx.tx, &wf_ids)
            .await?
            .into_iter()
            .collect();
    let ids: Vec<uuid::Uuid> = bills.iter().map(|b| b.id).collect();
    let (approvals, signals) = store::facts(&mut cx.tx, &ids).await?;
    let limits = store::auto_pay_limits(&mut cx.tx).await?;
    let builtin = Workflow::builtin_default();
    for bill in &bills {
        let wf = bill
            .workflow_id
            .and_then(|id| workflows.get(&id))
            .unwrap_or(&builtin);
        let a: Vec<_> = approvals
            .iter()
            .filter(|(b, _)| *b == bill.id)
            .map(|(_, a)| a.clone())
            .collect();
        let s: Vec<_> = signals
            .iter()
            .filter(|(b, _)| *b == bill.id)
            .map(|(_, s)| s.clone())
            .collect();
        let progress = workflow::evaluate(
            wf,
            &Facts {
                amount_cents: bill.amount_cents,
                received_at: bill.received_at,
                approvals: &a,
                signals: &s,
                released_at: bill.released_at,
                release_by_person: bill.release_by_person,
                payable_by_override: false,
                auto_pay_limit: limits.for_vendor(bill.vendor_id),
                now: cx.now,
            },
        );
        if let Status::AwaitingApproval {
            step_id,
            waiting_on,
        } = &progress.status
            && (waiting_on.is_empty() || waiting_on.iter().any(|w| w == actor))
        {
            return Err(CommandError::Conflict(format!(
                "bill {} is waiting on {actor}'s approval (step {step_id}); have them approve or reject it first — it keeps the workflow version it arrived under",
                bill.invoice_number
            )));
        }
    }
    Ok(())
}

async fn protect_last_admin(cx: &mut Cx<'_>, actor: &str) -> Result<(), CommandError> {
    let admins = store::sign_in_admins(&mut cx.tx, cx.org).await?;
    if admins.len() == 1 && admins[0] == actor {
        return Err(CommandError::Conflict(
            "this is the last admin who can sign in; give another person the Admin role first"
                .into(),
        ));
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRoles {
    actor: String,
    roles: Vec<Role>,
}
pub async fn set_roles(cx: &mut Cx<'_>, input: SetRoles) -> Result<Value, CommandError> {
    let actor = input.actor.trim().to_lowercase();
    let roles = validate_roles(input.roles)?;
    let person = store::people(&mut cx.tx, cx.org)
        .await?
        .into_iter()
        .find(|p| p.actor == actor)
        .ok_or(CommandError::NotFound("person"))?;
    let mut before = store::actor_roles(&mut cx.tx, cx.org, &actor).await?;
    before.sort();
    if before == roles {
        return Err(CommandError::Conflict(
            "these roles are already assigned".into(),
        ));
    }
    if !roles.contains(&Role::Admin) {
        protect_last_admin(cx, &actor).await?;
    }
    if !payvy_domain::permissions::Permission::Approve.allows(&roles) {
        approver_strand_refusal(cx, &actor).await?;
    }
    store::set_actor_roles(&mut cx.tx, cx.org, &actor, &roles).await?;
    cx.emit(
        "person.roles_changed",
        person.id,
        json!({ "actor": actor, "from": before, "to": roles }),
    )
    .await?;
    let mut person = json!(person);
    person["roles"] = json!(roles);
    Ok(json!({ "person": person }))
}

#[cfg(test)]
mod tests {
    use super::valid_actor;

    #[test]
    fn actor_names() {
        assert!(valid_actor("robert"));
        assert!(valid_actor("r.oneill-2"));
        assert!(!valid_actor(""));
        assert!(!valid_actor("-robert"));
        assert!(!valid_actor("Robert"));
        assert!(!valid_actor("rob ert"));
    }
}
