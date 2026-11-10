//! Ramp authorization staging. Credentials never implement Debug or Serialize.
use super::*;
use serde_json::Value;

#[derive(sqlx::FromRow)]
pub struct Authorization {
    pub id: Uuid,
    pub actor: String,
    pub environment: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub expires_at: DateTime<Utc>,
    pub status: String,
    pub credentials_sealed: Option<Vec<u8>>,
}

pub async fn insert(
    tx: &mut Tx,
    org: Uuid,
    state_hash: &str,
    a: &Authorization,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO ramp_authorizations (id, org_id, actor, state_hash, environment, client_id, redirect_uri, expires_at, status) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'pending')")
        .bind(a.id).bind(org).bind(&a.actor).bind(state_hash).bind(&a.environment).bind(&a.client_id).bind(&a.redirect_uri).bind(a.expires_at).execute(&mut **tx).await?;
    Ok(())
}

pub async fn by_state(tx: &mut Tx, hash: &str) -> anyhow::Result<Option<Authorization>> {
    Ok(sqlx::query_as("SELECT id, actor, environment, client_id, redirect_uri, expires_at, status, credentials_sealed FROM ramp_authorizations WHERE state_hash=$1 FOR UPDATE")
        .bind(hash).fetch_optional(&mut **tx).await?)
}
pub async fn get(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<Authorization>> {
    Ok(sqlx::query_as("SELECT id, actor, environment, client_id, redirect_uri, expires_at, status, credentials_sealed FROM ramp_authorizations WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut **tx).await?)
}
pub async fn finish(
    tx: &mut Tx,
    id: Uuid,
    status: &str,
    sealed: Option<&[u8]>,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE ramp_authorizations SET status=$2, credentials_sealed=$3 WHERE id=$1")
        .bind(id)
        .bind(status)
        .bind(sealed)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn insert_details(
    tx: &mut Tx,
    org: Uuid,
    id: Uuid,
    client_id: &str,
    details: &Value,
) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO ramp_connections (connection_id,org_id,client_id,details) VALUES ($1,$2,$3,$4)").bind(id).bind(org).bind(client_id).bind(details).execute(&mut **tx).await?;
    Ok(())
}
pub async fn details(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<(String, Value)>> {
    Ok(
        sqlx::query_as("SELECT client_id,details FROM ramp_connections WHERE connection_id=$1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?,
    )
}
pub async fn lock_connection(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<BankConnection>> {
    let row = sqlx::query(concat!(
        "SELECT ",
        connection_cols!(),
        " FROM bank_connections WHERE id=$1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(connection))
}
pub async fn update_credentials(tx: &mut Tx, id: Uuid, sealed: &[u8]) -> anyhow::Result<()> {
    sqlx::query("UPDATE bank_connections SET token_sealed=$2 WHERE id=$1")
        .bind(id)
        .bind(sealed)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
pub async fn set_instrument(
    tx: &mut Tx,
    id: Uuid,
    instrument: Uuid,
    fingerprint: &str,
) -> anyhow::Result<Payee> {
    let r = sqlx::query(concat!("UPDATE vendor_payees SET instrument_id=$2, instrument_fingerprint=$3 WHERE id=$1 RETURNING ", payee_cols!()))
        .bind(id).bind(instrument).bind(fingerprint).fetch_one(&mut **tx).await?;
    Ok(payee(&r))
}
