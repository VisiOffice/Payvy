use crate::{Role, Tx};
use uuid::Uuid;

pub async fn actor_roles(tx: &mut Tx, org: Uuid, actor: &str) -> anyhow::Result<Vec<Role>> {
    let roles: Option<Vec<String>> =
        sqlx::query_scalar("SELECT roles FROM actor_roles WHERE org_id = $1 AND actor = $2")
            .bind(org)
            .bind(actor)
            .fetch_optional(&mut **tx)
            .await?;
    roles
        .unwrap_or_default()
        .into_iter()
        .map(|s| Ok(serde_json::from_value(serde_json::Value::String(s))?))
        .collect()
}
pub async fn set_actor_roles(
    tx: &mut Tx,
    org: Uuid,
    actor: &str,
    roles: &[Role],
) -> anyhow::Result<()> {
    let roles: Vec<&str> = roles.iter().map(|r| r.as_str()).collect();
    sqlx::query("INSERT INTO actor_roles (org_id, actor, roles) VALUES ($1, $2, $3) ON CONFLICT (org_id, actor) DO UPDATE SET roles = EXCLUDED.roles")
        .bind(org).bind(actor).bind(roles).execute(&mut **tx).await?;
    Ok(())
}
/// Serialize membership/role changes before authorizing the caller, so two
/// admins cannot demote each other or remove the last sign-in admin.
pub async fn lock_roles(tx: &mut Tx, org: Uuid) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 170017))")
        .bind(org.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}
pub async fn sign_in_admins(tx: &mut Tx, org: Uuid) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT p.actor FROM people p JOIN actor_roles r ON r.org_id = p.org_id AND r.actor = p.actor WHERE p.org_id = $1 AND p.removed_at IS NULL AND 'admin' = ANY(r.roles)")
        .bind(org).fetch_all(&mut **tx).await?)
}
