//! Roles are read from storage on each call, not trusted from a cookie or
//! request body. The bootstrap server credential is explicitly distinguished
//! from a person named "admin". Agent permissions retain their registry gates.
use crate::{Actor, AppState};
use payvy_domain::{
    commands::{COMMANDS, CommandSpec},
    permissions::Role,
};
use payvy_store::{self as store, ActorKind};

pub async fn roles(tx: &mut store::Tx, actor: &Actor) -> anyhow::Result<Vec<Role>> {
    if actor.bootstrap {
        return Ok(vec![Role::Admin]);
    }
    store::actor_roles(tx, actor.org_id, &actor.name).await
}
pub fn allowed(actor: &Actor, roles: &[Role], spec: &CommandSpec) -> bool {
    if actor.kind == ActorKind::Agent {
        !spec.person_only
    } else {
        spec.permission.allows(roles)
    }
}
pub async fn visible(state: &AppState, actor: &Actor) -> anyhow::Result<Vec<&'static CommandSpec>> {
    let mut tx = state.store.begin(actor.org_id).await?;
    let roles = roles(&mut tx, actor).await?;
    Ok(COMMANDS
        .iter()
        .filter(|spec| allowed(actor, &roles, spec))
        .collect())
}
