//! Command-owned Ramp setup. There is deliberately no payment-write transport.
use super::{CommandError, Cx};
use chrono::{DateTime, Duration, Utc};
use payvy_pay::ramp::{self as api, Ramp, Tokens};
use payvy_store::{self as store, ramp as db};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

impl From<api::Error> for CommandError {
    fn from(e: api::Error) -> Self {
        Self::Bank(e.to_string())
    }
}
fn conflict(message: &str) -> CommandError {
    CommandError::Conflict(message.into())
}
pub(super) fn config<'a>(cx: &'a Cx<'_>) -> Result<&'a crate::ramp::Config, CommandError> {
    cx.bank()?.ramp.as_ref().ok_or(CommandError::NotConfigured(
        "Ramp OAuth is not configured on this server",
    ))
}
fn hash(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}
fn stage_purpose(id: Uuid) -> String {
    format!("ramp-authorization:{id}")
}
pub(super) fn bank_purpose(id: Uuid) -> String {
    format!("bank:{id}")
}
fn deadline(now: DateTime<Utc>, seconds: u64) -> Result<DateTime<Utc>, CommandError> {
    i64::try_from(seconds)
        .ok()
        .and_then(Duration::try_seconds)
        .and_then(|d| now.checked_add_signed(d))
        .ok_or(api::Error::InvalidResponse.into())
}

// Serialize ONLY as plaintext input to the sealer, never as a command result.
#[derive(Serialize, Deserialize)]
pub(super) struct Credentials {
    pub(super) access: String,
    pub(super) access_expires: DateTime<Utc>,
    pub(super) refresh: Option<String>,
    pub(super) refresh_expires: Option<DateTime<Utc>>,
}
impl Credentials {
    pub(super) fn from_tokens(t: Tokens, now: DateTime<Utc>) -> Result<Self, CommandError> {
        for scope in api::DISCOVERY_SCOPES.split_whitespace() {
            if !t.scope.split_whitespace().any(|s| s == scope) {
                return Err(api::Error::Unauthorized.into());
            }
        }
        Ok(Self {
            access: t.access_token,
            access_expires: deadline(now, t.expires_in)?,
            refresh: t.refresh_token,
            refresh_expires: t
                .refresh_token_expires_in
                .map(|s| deadline(now, s))
                .transpose()?,
        })
    }
    fn client(&self, cx: &Cx<'_>) -> Result<Ramp, CommandError> {
        if self.access_expires <= cx.now + Duration::seconds(30) {
            return Err(conflict(
                "Ramp access expired; renew access in Bank settings or reconnect Ramp",
            ));
        }
        Ok(config(cx)?.client(&self.access)?)
    }
    pub(super) fn seal(&self, cx: &Cx<'_>, purpose: &str) -> Result<Vec<u8>, CommandError> {
        Ok(cx.bank()?.sealer.seal(
            cx.org,
            purpose,
            &serde_json::to_string(self).map_err(anyhow::Error::from)?,
        )?)
    }
    pub(super) fn open(cx: &Cx<'_>, purpose: &str, sealed: &[u8]) -> Result<Self, CommandError> {
        let raw = cx.bank()?.sealer.open(cx.org, purpose, sealed)?;
        serde_json::from_str(&raw)
            .map_err(|_| conflict("Ramp credentials could not be read; reconnect Ramp"))
    }
}
fn check_stage(cx: &Cx<'_>, a: &db::Authorization, status: &str) -> Result<(), CommandError> {
    let conf = config(cx)?;
    if a.actor != cx.actor.name || a.expires_at <= cx.now || a.status != status {
        return Err(conflict(
            "Ramp authorization is expired, already used, or belongs to another person; start again",
        ));
    }
    if a.environment != conf.environment_name()
        || a.client_id != conf.client_id
        || a.redirect_uri != conf.redirect_uri
    {
        return Err(conflict(
            "Ramp app configuration changed; start authorization again",
        ));
    }
    Ok(())
}
async fn stage(cx: &mut Cx<'_>, id: Uuid) -> Result<db::Authorization, CommandError> {
    let a = db::get(&mut cx.tx, id)
        .await?
        .ok_or(CommandError::NotFound("Ramp authorization"))?;
    check_stage(cx, &a, "ready")?;
    Ok(a)
}
fn stage_credentials(cx: &Cx<'_>, a: &db::Authorization) -> Result<Credentials, CommandError> {
    Credentials::open(
        cx,
        &stage_purpose(a.id),
        a.credentials_sealed
            .as_deref()
            .ok_or_else(|| conflict("Ramp authorization is not ready"))?,
    )
}

pub async fn authorize(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let conf = config(cx)?;
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|_| anyhow::anyhow!("could not generate OAuth state"))?;
    let state = hex::encode(random);
    let url = conf
        .oauth()?
        .authorize_url(conf.environment, &conf.redirect_uri, &state)?;
    let a = db::Authorization {
        id: Uuid::new_v4(),
        actor: cx.actor.name.clone(),
        environment: conf.environment_name().into(),
        client_id: conf.client_id.clone(),
        redirect_uri: conf.redirect_uri.clone(),
        expires_at: cx.now + Duration::minutes(10),
        status: "pending".into(),
        credentials_sealed: None,
    };
    db::insert(&mut cx.tx, cx.org, &hash(state.as_bytes()), &a).await?;
    cx.emit(
        "bank.authorization_started",
        a.id,
        json!({"provider":"ramp","authorization_id":a.id,"environment":a.environment}),
    )
    .await?;
    Ok(
        json!({"authorization_id":a.id,"authorize_url":if cx.preview {None} else {Some(url)},"expires_at":a.expires_at}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Complete {
    state: String,
    code: String,
}
pub async fn complete(cx: &mut Cx<'_>, input: Complete) -> Result<Value, CommandError> {
    let a = db::by_state(&mut cx.tx, &hash(input.state.as_bytes()))
        .await?
        .ok_or(CommandError::NotFound("Ramp authorization"))?;
    check_stage(cx, &a, "pending")?;
    if cx.preview {
        return Ok(
            json!({"authorization_id":a.id,"status":"pending","note":"Preview does not exchange the authorization code"}),
        );
    }
    // A failed/unclear exchange consumes this state, rather than automatically retrying a code.
    let conf = config(cx)?;
    let result = conf
        .oauth()?
        .authorization_code(&input.code, &a.redirect_uri)
        .await
        .map_err(CommandError::from)
        .and_then(|t| Credentials::from_tokens(t, cx.now));
    match result {
        Ok(credentials) => {
            let sealed = credentials.seal(cx, &stage_purpose(a.id))?;
            db::finish(&mut cx.tx, a.id, "ready", Some(&sealed)).await?;
            cx.emit(
                "bank.authorization_completed",
                a.id,
                json!({"provider":"ramp","authorization_id":a.id}),
            )
            .await?;
            Ok(
                json!({"authorization_id":a.id,"status":"ready","next":"Choose an entity and funding account with bank.authorization, then preview bank.activate"}),
            )
        }
        Err(error) => {
            db::finish(&mut cx.tx, a.id, "failed", None).await?;
            cx.emit(
                "bank.authorization_failed",
                a.id,
                json!({"provider":"ramp","authorization_id":a.id}),
            )
            .await?;
            Ok(
                json!({"authorization_id":a.id,"status":"failed","error":error.to_string(),"next":"Start a new authorization"}),
            )
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authorization {
    authorization_id: Uuid,
}

async fn choices(client: &Ramp) -> Result<(api::Business, Vec<Value>), CommandError> {
    let business = client.business().await?;
    if !business.active {
        return Err(conflict("This Ramp business is inactive"));
    }
    let entities = client.entities().await?;
    let accounts = client.accounts().await?;
    let mut choices = Vec::new();
    for entity in entities.iter().filter(|e| e.currency == "USD") {
        for ea in entity
            .payment_accounts
            .iter()
            .filter(|a| a.currency == "USD")
        {
            let Some(account) = accounts.iter().find(|a| {
                a.id == ea.id
                    && a.has_ach_details
                    && a.account_number_last_four == ea.account_number_last_four
            }) else {
                continue;
            };
            let mut details = json!({"business_id":business.id,"business_name":business.business_name_legal,
                "entity_id":entity.id,"entity_name":entity.entity_name,"account_id":account.id,
                "account_name":account.account_name,"entity_account_name":ea.account_name,
                "destination":account.account_number_last_four.masked(),"currency":"USD",
                "institution":account.financial_institution_name});
            let fingerprint = hash(&serde_json::to_vec(&details).map_err(anyhow::Error::from)?);
            details["fingerprint"] = json!(fingerprint);
            choices.push(details);
        }
    }
    Ok((business, choices))
}
pub async fn authorization(cx: &mut Cx<'_>, input: Authorization) -> Result<Value, CommandError> {
    let a = stage(cx, input.authorization_id).await?;
    let client = stage_credentials(cx, &a)?.client(cx)?;
    let (business, choices) = choices(&client).await?;
    let previous = store::live_paying_connection(&mut cx.tx).await?;
    Ok(
        json!({"authorization_id":a.id,"environment":a.environment,"business":business,"choices":choices,"replacing":previous,"expires_at":a.expires_at,"payments_enabled":false}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activate {
    authorization_id: Uuid,
    entity_id: Uuid,
    account_id: Uuid,
    fingerprint: String,
    // Required, explicit null when there is no connection to replace.
    #[serde(deserialize_with = "required_option")]
    replacing_connection_id: Option<Uuid>,
}
fn required_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Uuid>, D::Error> {
    Option::deserialize(d)
}

pub async fn activate(cx: &mut Cx<'_>, input: Activate) -> Result<Value, CommandError> {
    store::lock_bank(&mut cx.tx, cx.org).await?;
    let a = stage(cx, input.authorization_id).await?;
    let credentials = stage_credentials(cx, &a)?;
    let (_, choices) = choices(&credentials.client(cx)?).await?;
    let selected = choices
        .into_iter()
        .find(|c| {
            c["entity_id"] == input.entity_id.to_string()
                && c["account_id"] == input.account_id.to_string()
                && c["fingerprint"] == input.fingerprint
        })
        .ok_or_else(|| {
            conflict("Ramp funding details changed or this account is ineligible; choose again")
        })?;
    let previous = store::live_paying_connection(&mut cx.tx).await?;
    if previous.as_ref().map(|c| c.id) != input.replacing_connection_id {
        return Err(conflict(
            "The current bank connection changed; review the replacement again",
        ));
    }
    let id = Uuid::new_v4();
    let sealed = credentials.seal(cx, &bank_purpose(id))?;
    store::retire_paying_connection(&mut cx.tx, cx.now, &cx.actor.name).await?;
    let account_name = format!(
        "{} · {} {}",
        selected["entity_name"].as_str().unwrap_or(""),
        selected["account_name"].as_str().unwrap_or(""),
        selected["destination"].as_str().unwrap_or("")
    );
    let c = store::insert_connection(
        &mut cx.tx,
        cx.org,
        &store::NewConnection {
            id,
            provider: "ramp",
            environment: &a.environment,
            mode: "setup",
            account_id: &input.account_id.to_string(),
            account_name: &account_name,
            token_sealed: &sealed,
            created_at: cx.now,
            created_by: &cx.actor.name,
        },
    )
    .await?;
    db::insert_details(&mut cx.tx, cx.org, id, &a.client_id, &selected).await?;
    db::set_health(
        &mut cx.tx,
        id,
        &super::ramp_health::connected(cx.now, credentials.access_expires),
    )
    .await?;
    db::finish(&mut cx.tx, a.id, "used", None).await?;
    // Match Mercury replacement: remove its old webhook, while polling keeps
    // tracking outstanding payments through their retained connection. The
    // helper skips previews and treats provider cleanup failures as nonfatal.
    if let Some(old) = &previous {
        super::bank::remove_webhook(cx, old).await;
    }
    cx.emit("bank.connected",id,json!({"provider":"ramp","connection_id":id,"details":selected,"replaced":input.replacing_connection_id,"payments_enabled":false})).await?;
    Ok(
        json!({"connection":c,"details":selected,"replacing":previous,"payments_enabled":false,"note":"Ramp is selected for setup and payee mapping. Sending and auto-pay are disabled. Existing payees must be mapped again for this connection."}),
    )
}

pub(super) async fn client(
    cx: &mut Cx<'_>,
    c: &store::BankConnection,
) -> Result<Ramp, CommandError> {
    if c.provider != "ramp" {
        return Err(conflict("This is not a Ramp connection"));
    }
    let (app, _) = db::details(&mut cx.tx, c.id)
        .await?
        .ok_or(CommandError::NotFound("Ramp connection details"))?;
    if config(cx)?.environment_name() != c.environment || config(cx)?.client_id != app {
        return Err(conflict("Ramp app configuration changed; reconnect Ramp"));
    }
    let health = db::health(&mut cx.tx, c.id)
        .await?
        .ok_or(CommandError::NotFound("Ramp connection health"))?;
    if health.health_status == "needs_reconnection" {
        return Err(conflict(
            "Reconnect Ramp in Bank settings to restore access",
        ));
    }
    if health.health_status == "paused"
        || health.next_attempt_at.is_some_and(|at| at > cx.now)
            && health.health_status == "retrying"
    {
        return Err(conflict(
            "Ramp access checks are paused or waiting to retry. See Bank settings for the retry time.",
        ));
    }
    Credentials::open(cx, &bank_purpose(c.id), &c.token_sealed)?.client(cx)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Renew {
    pub(super) connection_id: Uuid,
}
pub async fn renew(cx: &mut Cx<'_>, input: Renew) -> Result<Value, CommandError> {
    super::ramp_health::maintain(cx, input, true).await
}

pub(super) async fn destinations(
    cx: &mut Cx<'_>,
    c: &store::BankConnection,
    vendor_id: Uuid,
) -> Result<Vec<Value>, CommandError> {
    let client = client(cx, c).await?;
    let vendor = client.vendor(vendor_id).await?;
    if !vendor.is_active {
        return Err(conflict("This Ramp vendor is inactive"));
    }
    client.ach_instruments(vendor_id).await?.into_iter().map(|instrument| {
        let mut details = json!({"connection_id":c.id,"recipient_id":vendor.id,"recipient_name":vendor.name,"instrument_id":instrument.id,"method":"ach","destination":instrument.account_number_last_four.masked(),"fee":instrument.fee});
        let fingerprint = hash(&serde_json::to_vec(&details).map_err(anyhow::Error::from)?);
        details["fingerprint"] = json!(fingerprint);
        Ok(details)
    }).collect()
}
pub(super) async fn destination(
    cx: &mut Cx<'_>,
    c: &store::BankConnection,
    vendor_id: Uuid,
    instrument_id: Uuid,
) -> Result<Value, CommandError> {
    destinations(cx, c, vendor_id)
        .await?
        .into_iter()
        .find(|d| d["instrument_id"] == instrument_id.to_string())
        .ok_or_else(|| conflict("This Ramp ACH account is no longer available"))
}
