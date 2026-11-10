//! The customer's bank: connecting it, and who each vendor is paid to.
//! Payvy keeps the bank's API token sealed and the recipient's id; account
//! numbers stay in the bank, and people see only the last four digits.

use payvy_pay::{BankError, Mercury, Method, Mode, Rho, RhoTransactionQuery};
use payvy_store::{self as store, BankConnection, NewConnection, NewPayee};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, text};

impl From<BankError> for CommandError {
    fn from(e: BankError) -> Self {
        match e {
            BankError::Unauthorized(m) => CommandError::Bank(format!(
                "the bank refused Payvy's token ({m}); a person reconnects with bank.connect"
            )),
            BankError::Refused(m) => CommandError::Bank(format!("the bank refused: {m}")),
            BankError::Unclear(m) => {
                CommandError::Bank(format!("the bank didn't answer clearly: {m}"))
            }
        }
    }
}

fn sealed_as(connection: Uuid) -> String {
    format!("bank:{connection}")
}

/// Registers a webhook for a new production Mercury connection, so payment
/// updates arrive as they happen. Never fails the connection: without it,
/// the payment checker still asks Mercury every minute.
async fn register_webhook(cx: &mut Cx<'_>, c: &BankConnection, client: &Mercury) -> Value {
    let Some(base) = cx.bank().ok().and_then(|b| b.webhook_base.clone()) else {
        return json!(
            "Payvy checks payments with Mercury every minute (no webhook address is configured)."
        );
    };
    if c.environment != "production" {
        return json!(
            "Payvy checks payments with Mercury every minute (Mercury's sandbox has no webhooks)."
        );
    }
    let url = format!("{base}/webhooks/mercury/{}/{}", cx.org, c.id);
    let result = match client.create_webhook(&url).await {
        Ok(w) => match cx.bank() {
            Ok(bank) => {
                match bank
                    .sealer
                    .seal(cx.org, &crate::mercury_webhook::sealed_as(c.id), &w.secret)
                {
                    Ok(sealed) => store::set_connection_webhook(&mut cx.tx, c.id, &w.id, &sealed)
                        .await
                        .map(|()| w.id)
                        .map_err(|e| format!("{e:#}")),
                    Err(e) => Err(format!("{e:#}")),
                }
            }
            Err(_) => Err("bank settings unavailable".into()),
        },
        Err(BankError::Unauthorized(m) | BankError::Refused(m) | BankError::Unclear(m)) => Err(m),
    };
    match result {
        Ok(id) => {
            tracing::info!(connection = %c.id, webhook = %id, "Mercury webhook registered");
            json!(
                "Mercury tells Payvy about payments as they happen; Payvy also checks every minute."
            )
        }
        Err(m) => {
            tracing::warn!(connection = %c.id, "Mercury webhook not registered: {m}");
            json!(format!(
                "Payvy checks payments with Mercury every minute. Mercury didn't accept a webhook ({m}); a token allowed to manage webhooks gets updates as they happen."
            ))
        }
    }
}

/// Removes the webhook of a connection being retired. Best effort: a
/// leftover endpoint's deliveries are refused once its connection is gone.
async fn remove_webhook(cx: &Cx<'_>, c: &BankConnection) {
    let Some(id) = &c.webhook_id else { return };
    if cx.preview {
        return;
    }
    let result = match mercury(cx, c) {
        Ok(client) => client
            .delete_webhook(id)
            .await
            .map_err(|e| format!("{e:?}")),
        Err(e) => Err(format!("{e:?}")),
    };
    if let Err(e) = result {
        tracing::warn!(connection = %c.id, webhook = %id, "couldn't remove Mercury webhook: {e}");
    }
}

/// A client for this connection, with its token unsealed.
pub(super) fn mercury(cx: &Cx<'_>, c: &BankConnection) -> Result<Mercury, CommandError> {
    if c.provider != "mercury" {
        return Err(CommandError::Conflict(
            "This operation requires Mercury; Ramp payments are not enabled".into(),
        ));
    }
    let bank = cx.bank()?;
    let token = bank
        .sealer
        .open(cx.org, &sealed_as(c.id), &c.token_sealed)?;
    let base = match c.environment.as_str() {
        "sandbox" => &bank.mercury_sandbox,
        _ => &bank.mercury_production,
    };
    Ok(Mercury::new(base, &token)?)
}

pub(super) async fn live(cx: &mut Cx<'_>) -> Result<BankConnection, CommandError> {
    store::live_paying_connection(&mut cx.tx)
        .await?
        .ok_or_else(|| {
            CommandError::Conflict(
                "no paying bank is connected; a person connects one with bank.connect".into(),
            )
        })
}

/// A client for the live read-only Rho connection, with its token unsealed.
pub(super) fn rho_client(cx: &Cx<'_>, c: &BankConnection) -> Result<Rho, CommandError> {
    let bank = cx.bank()?;
    let token = bank
        .sealer
        .open(cx.org, &sealed_as(c.id), &c.token_sealed)?;
    let base = match c.environment.as_str() {
        "sandbox" => &bank.rho_sandbox,
        _ => &bank.rho_production,
    };
    Ok(Rho::new(base, &token)?)
}

pub(super) async fn live_rho(cx: &mut Cx<'_>) -> Result<BankConnection, CommandError> {
    store::live_read_connection(&mut cx.tx, "rho")
        .await?
        .ok_or_else(|| {
            CommandError::Conflict(
                "no read-only Rho connection; a person connects one with bank.connect".into(),
            )
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connect {
    provider: String,
    token: String,
    environment: Option<String>,
    mode: Option<String>,
    account_id: Option<String>,
}

/// Rho is read-only: it validates with accounts:read, never retires the
/// paying connection, and stores mode read_only so the pay path can't
/// pick it up by mistake.
async fn connect_rho(cx: &mut Cx<'_>, input: &Connect) -> Result<Value, CommandError> {
    let bank = cx.bank()?;
    if let Some(m) = input.mode.as_deref()
        && m != "read_only"
    {
        return Err(CommandError::Invalid(
            "Rho is read-only; mode is read_only".into(),
        ));
    }
    let environment = input.environment.as_deref().unwrap_or("production");
    let base = match environment {
        "production" => &bank.rho_production,
        "sandbox" => &bank.rho_sandbox,
        _ => {
            return Err(CommandError::Invalid(
                "environment is production or sandbox".into(),
            ));
        }
    };
    let token = input.token.trim();
    if token.is_empty() {
        return Err(CommandError::Invalid("token can't be empty".into()));
    }
    let client = Rho::new(base, token)?;
    let accounts = client.accounts().await.map_err(|e| match e {
        BankError::Unauthorized(m) => CommandError::Invalid(format!(
            "Rho refused this token ({m}); create one in Rho Settings > API > Access Tokens with accounts:read and transactions:read, or reconnect — tokens expire after 45 days unused"
        )),
        e => e.into(),
    })?;
    let account = match &input.account_id {
        Some(id) => accounts
            .iter()
            .find(|a| a.id == *id)
            .ok_or_else(|| CommandError::Invalid(format!("no Rho account {id}")))?,
        None if accounts.len() == 1 => &accounts[0],
        None if accounts.is_empty() => {
            return Err(CommandError::Invalid(
                "Rho returned no accounts for this token".into(),
            ));
        }
        // Several accounts: the first is the display account; every
        // account stays readable through bank.accounts.
        None => &accounts[0],
    };
    let id = Uuid::new_v4();
    let sealed = bank.sealer.seal(cx.org, &sealed_as(id), token)?;
    let previous = store::live_read_connection(&mut cx.tx, "rho").await?;
    let replaced = match &previous {
        Some(p) => store::retire_connection_by_id(&mut cx.tx, p.id, cx.now, &cx.actor.name).await?,
        None => false,
    };
    let c = store::insert_connection(
        &mut cx.tx,
        cx.org,
        &NewConnection {
            id,
            provider: "rho",
            environment,
            mode: "read_only",
            account_id: &account.id,
            account_name: &account.name,
            token_sealed: &sealed,
            created_at: cx.now,
            created_by: &cx.actor.name,
        },
    )
    .await?;
    cx.emit(
        "bank.connected",
        c.id,
        json!({ "connection_id": c.id, "provider": "rho", "environment": environment,
                "mode": "read_only", "capability": "read_only", "account_name": account.name,
                "replaced_previous": replaced }),
    )
    .await?;
    Ok(json!({ "connection": c, "capability": "read_only",
        "display_account": { "id": account.id, "name": account.name },
        "note": if input.account_id.is_none() && accounts.len() > 1 {
            format!("{} is the display account (the first of {} Rho accounts); every account stays readable with bank.accounts. Pass account_id to choose another.", account.name, accounts.len())
        } else {
            format!("{} is the display account; every account stays readable with bank.accounts.", account.name)
        },
        "how_it_works": "Rho is read-only in Payvy: it shows accounts, balances and transactions (bank.accounts, bank.transactions). It cannot pay bills — releases still go through a paying bank such as Mercury." }))
}

pub async fn connect(cx: &mut Cx<'_>, input: Connect) -> Result<Value, CommandError> {
    store::lock_bank(&mut cx.tx, cx.org).await?;
    let bank = cx.bank()?;
    if input.provider == "rho" {
        return connect_rho(cx, &input).await;
    }
    if input.provider != "mercury" {
        return Err(CommandError::Invalid(
            "provider is mercury or rho (Ramp, Brex, Wise and Airwallex are planned)".into(),
        ));
    }
    let environment = input.environment.unwrap_or_else(|| "production".into());
    let base = match environment.as_str() {
        "production" => &bank.mercury_production,
        "sandbox" => &bank.mercury_sandbox,
        _ => {
            return Err(CommandError::Invalid(
                "environment is production or sandbox".into(),
            ));
        }
    };
    let mode = match input.mode.as_deref() {
        None => Mode::Request,
        Some(m) => Mode::parse(m)
            .ok_or_else(|| CommandError::Invalid("mode is request or direct".into()))?,
    };
    let token = input.token.trim();
    if token.is_empty() {
        return Err(CommandError::Invalid("token can't be empty".into()));
    }
    let client = Mercury::new(base, token)?;
    let accounts = client.accounts().await.map_err(|e| match e {
        BankError::Unauthorized(m) | BankError::Refused(m) => {
            CommandError::Invalid(format!("Mercury refused this token: {m}"))
        }
        e => e.into(),
    })?;
    let active: Vec<_> = accounts.iter().filter(|a| a.status == "active").collect();
    let account = match &input.account_id {
        Some(id) => active
            .iter()
            .find(|a| a.id == *id)
            .ok_or_else(|| CommandError::Invalid(format!("no active Mercury account {id}")))?,
        None if active.len() == 1 => &active[0],
        None => {
            let names: Vec<String> = active
                .iter()
                .map(|a| format!("{} ({})", a.name, a.id))
                .collect();
            return Err(CommandError::Invalid(format!(
                "pick the account bills are paid from with account_id: {}",
                if names.is_empty() {
                    "none are active".into()
                } else {
                    names.join(", ")
                }
            )));
        }
    };
    let id = Uuid::new_v4();
    let sealed = bank.sealer.seal(cx.org, &sealed_as(id), token)?;
    let previous = store::live_paying_connection(&mut cx.tx).await?;
    let replaced = store::retire_paying_connection(&mut cx.tx, cx.now, &cx.actor.name).await?;
    if let Some(old) = &previous {
        remove_webhook(cx, old).await;
    }
    let c = store::insert_connection(
        &mut cx.tx,
        cx.org,
        &NewConnection {
            id,
            provider: "mercury",
            environment: &environment,
            mode: mode.as_str(),
            account_id: &account.id,
            account_name: &account.name,
            token_sealed: &sealed,
            created_at: cx.now,
            created_by: &cx.actor.name,
        },
    )
    .await?;
    cx.emit(
        "bank.connected",
        c.id,
        json!({ "connection_id": c.id, "provider": "mercury", "environment": environment,
                "mode": mode.as_str(), "account_name": account.name, "replaced_previous": replaced }),
    )
    .await?;
    let updates = if cx.preview {
        json!("On connecting, Payvy asks Mercury to tell it about payments as they happen.")
    } else {
        register_webhook(cx, &c, &client).await
    };
    let c = store::get_connection(&mut cx.tx, c.id).await?.unwrap_or(c);
    let how = match mode {
        Mode::Request => {
            "Payments wait in Mercury for approval by a Mercury admin other than the token's creator. The token needs the RequestSendMoney scope."
        }
        Mode::Direct => {
            "Mercury sends payments as soon as a person releases them in Payvy. The token must allow this server's IP address."
        }
    };
    Ok(json!({ "connection": c, "how_payments_work": how, "payment_updates": updates }))
}

fn capability(c: &BankConnection) -> &'static str {
    if c.mode == "setup" {
        "setup"
    } else if c.mode == "read_only" {
        "read_only"
    } else {
        "pay"
    }
}

pub async fn get(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let paying = store::live_paying_connection(&mut cx.tx).await?;
    let all = store::live_connections(&mut cx.tx).await?;
    let connections: Vec<Value> = all
        .iter()
        .map(|c| {
            let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
            v["capability"] = json!(capability(c));
            v
        })
        .collect();
    Ok(
        json!({ "connection": paying, "connections": connections, "ramp_available": cx.bank().ok().is_some_and(|b| b.ramp.is_some()) }),
    )
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Disconnect {
    provider: Option<String>,
}

pub async fn disconnect(cx: &mut Cx<'_>, input: Disconnect) -> Result<Value, CommandError> {
    store::lock_bank(&mut cx.tx, cx.org).await?;
    let c = match input.provider.as_deref() {
        None => live(cx).await?,
        Some("rho") => live_rho(cx).await?,
        Some(p) => {
            let paying = live(cx).await?;
            if paying.provider != p {
                return Err(CommandError::NotFound("live bank connection"));
            }
            paying
        }
    };
    store::retire_connection_by_id(&mut cx.tx, c.id, cx.now, &cx.actor.name).await?;
    remove_webhook(cx, &c).await;
    cx.emit(
        "bank.disconnected",
        c.id,
        json!({ "connection_id": c.id, "provider": c.provider, "account_name": c.account_name }),
    )
    .await?;
    Ok(json!({
        "disconnected": c.id,
        "provider": c.provider,
        "note": if c.mode == "read_only" {
            "This read-only connection stops reporting. Nothing about payments changes."
        } else {
            "No new payments are sent. Payments already sent are still tracked until they finish."
        },
    }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Accounts {}

pub async fn accounts(cx: &mut Cx<'_>, _input: Accounts) -> Result<Value, CommandError> {
    let c = live_rho(cx).await?;
    let accounts = rho_client(cx, &c)?.accounts().await?;
    Ok(json!({ "provider": "rho", "accounts": accounts }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Transactions {
    account_id: Option<String>,
    posted_after: Option<String>,
    posted_before: Option<String>,
    initiated_after: Option<String>,
    initiated_before: Option<String>,
    search: Option<String>,
    limit: Option<usize>,
}

pub async fn transactions(cx: &mut Cx<'_>, input: Transactions) -> Result<Value, CommandError> {
    let c = live_rho(cx).await?;
    let txns = rho_client(cx, &c)?
        .transactions(&RhoTransactionQuery {
            account_id: text(input.account_id),
            posted_after: text(input.posted_after),
            posted_before: text(input.posted_before),
            initiated_after: text(input.initiated_after),
            initiated_before: text(input.initiated_before),
            search: text(input.search),
            limit: input.limit.unwrap_or(100),
        })
        .await?;
    Ok(json!({ "provider": "rho", "transactions": txns }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipients {
    search: Option<String>,
    recipient_id: Option<Uuid>,
}

pub async fn recipients(cx: &mut Cx<'_>, input: Recipients) -> Result<Value, CommandError> {
    let c = live(cx).await?;
    if c.provider == "ramp" {
        let client = super::ramp::client(cx, &c).await?;
        if let Some(id) = input.recipient_id {
            let destinations = super::ramp::destinations(cx, &c, id).await?;
            return Ok(json!({"provider":"ramp","connection_id":c.id,"destinations":destinations}));
        }
        let needle = text(input.search).map(|s| s.to_lowercase());
        let vendors: Vec<_> = client
            .vendors()
            .await?
            .into_iter()
            .filter(|v| {
                v.is_active
                    && needle
                        .as_ref()
                        .is_none_or(|n| v.name.to_lowercase().contains(n))
            })
            .collect();
        return Ok(json!({"provider":"ramp","connection_id":c.id,"vendors":vendors}));
    }
    let all = mercury(cx, &c)?.recipients().await?;
    let needle = text(input.search).map(|s| s.to_lowercase());
    let found: Vec<_> = all
        .into_iter()
        .filter(|r| {
            needle
                .as_deref()
                .is_none_or(|n| r.name.to_lowercase().contains(n))
        })
        .collect();
    Ok(json!({ "recipients": found }))
}

// ── Payees ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposePayee {
    connection_id: Option<Uuid>,
    instrument_id: Option<Uuid>,
    instrument_fingerprint: Option<String>,
    vendor_id: Uuid,
    recipient_id: String,
    method: String,
    reason: Option<String>,
}

pub async fn propose_payee(cx: &mut Cx<'_>, input: ProposePayee) -> Result<Value, CommandError> {
    store::lock_bank(&mut cx.tx, cx.org).await?;
    let method = Method::parse(&input.method).ok_or_else(|| {
        CommandError::Invalid("method is ach, check, domestic_wire or real_time_payment".into())
    })?;
    let vendor = store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let c = live(cx).await?;
    if c.provider == "ramp" {
        if input.method != "ach" || input.connection_id != Some(c.id) {
            return Err(CommandError::Invalid(
                "Choose an ACH account on the current Ramp connection".into(),
            ));
        }
        let instrument = input
            .instrument_id
            .ok_or_else(|| CommandError::Invalid("Ramp needs an explicit instrument_id".into()))?;
        let recipient = Uuid::parse_str(&input.recipient_id)
            .map_err(|_| CommandError::Invalid("Ramp recipient_id must be a UUID".into()))?;
        let details = super::ramp::destination(cx, &c, recipient, instrument).await?;
        if details["fingerprint"].as_str() != input.instrument_fingerprint.as_deref() {
            return Err(CommandError::Conflict(
                "Ramp destination changed; choose the account again".into(),
            ));
        }
        let fingerprint = details["fingerprint"].as_str().unwrap();
        let p = store::insert_payee_proposal(
            &mut cx.tx,
            cx.org,
            &NewPayee {
                vendor_id: vendor.id,
                connection_id: c.id,
                recipient_id: &recipient.to_string(),
                recipient_name: details["recipient_name"].as_str().unwrap(),
                method: "ach",
                destination: details["destination"].as_str().unwrap(),
                reason: text(input.reason).as_deref(),
                proposed_by: &cx.actor.name,
                created_at: cx.now,
            },
        )
        .await?;
        let p = store::ramp::set_instrument(&mut cx.tx, p.id, instrument, fingerprint).await?;
        cx.emit(
            "vendor.payee_proposed",
            vendor.id,
            json!({"vendor_id":vendor.id,"proposal_id":p.id,"provider":"ramp","details":details}),
        )
        .await?;
        return Ok(
            json!({"proposal":p,"next":"A person verifies the destination and accepts it with vendor.accept_payee. Ramp payments remain disabled."}),
        );
    }
    if input.instrument_id.is_some()
        || input.instrument_fingerprint.is_some()
        || input.connection_id.is_some_and(|id| id != c.id)
    {
        return Err(CommandError::Invalid(
            "These mapping details do not match the current bank".into(),
        ));
    }
    let recipient = mercury(cx, &c)?
        .recipient_by_id(input.recipient_id.trim())
        .await?
        .ok_or(CommandError::NotFound("recipient in the bank"))?;
    if !recipient.active {
        return Err(CommandError::Invalid(format!(
            "{} is deleted in the bank",
            recipient.name
        )));
    }
    let Some(destination) = recipient.destination(method) else {
        let can: Vec<&str> = recipient
            .methods
            .iter()
            .map(|m| m.method.as_str())
            .collect();
        return Err(CommandError::Invalid(format!(
            "{} has no {} details in the bank; it can be paid by {}",
            recipient.name,
            method.as_str(),
            if can.is_empty() {
                "nothing yet".into()
            } else {
                can.join(", ")
            }
        )));
    };
    let p = store::insert_payee_proposal(
        &mut cx.tx,
        cx.org,
        &NewPayee {
            vendor_id: vendor.id,
            connection_id: c.id,
            recipient_id: &recipient.id,
            recipient_name: &recipient.name,
            method: method.as_str(),
            destination,
            reason: text(input.reason).as_deref(),
            proposed_by: &cx.actor.name,
            created_at: cx.now,
        },
    )
    .await?;
    cx.emit(
        "vendor.payee_proposed",
        vendor.id,
        json!({ "vendor_id": vendor.id, "proposal_id": p.id, "recipient_name": p.recipient_name,
                "method": p.method, "destination": p.destination }),
    )
    .await?;
    Ok(json!({ "proposal": p, "next": "a person accepts it with vendor.accept_payee" }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRecipient {
    vendor_id: Uuid,
    method: String,
    /// Defaults to the vendor's name.
    name: Option<String>,
    /// Defaults to the vendor's email; Mercury requires one.
    email: Option<String>,
    #[serde(default)]
    account_number: String,
    #[serde(default)]
    routing_number: String,
    /// ACH only: business_checking (default), business_savings,
    /// personal_checking or personal_savings.
    account_type: Option<String>,
    address: payvy_pay::Address,
}

/// ABA routing numbers carry a check digit: 3·(d1+d4+d7) + 7·(d2+d5+d8) +
/// (d3+d6+d9) is a multiple of 10.
fn routing_ok(r: &str) -> bool {
    let d: Vec<u32> = r.chars().filter_map(|c| c.to_digit(10)).collect();
    r.len() == 9
        && d.len() == 9
        && (3 * (d[0] + d[3] + d[6]) + 7 * (d[1] + d[4] + d[7]) + (d[2] + d[5] + d[8]))
            .is_multiple_of(10)
}

fn last4(n: &str) -> String {
    format!("…{}", &n[n.len().saturating_sub(4)..])
}

/// A person types a vendor's bank details; Payvy creates the recipient in
/// Mercury and proposes it as the vendor's payee, which a person still
/// accepts. The numbers go straight to Mercury: they are never stored,
/// logged, or put in a result or event. Mercury has no idempotency key for
/// this, so an unclear answer is never retried.
pub async fn create_recipient(
    cx: &mut Cx<'_>,
    input: CreateRecipient,
) -> Result<Value, CommandError> {
    let method = Method::parse(&input.method).ok_or_else(|| {
        CommandError::Invalid("method is ach, check, domestic_wire or real_time_payment".into())
    })?;
    let vendor = store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let name = text(input.name).unwrap_or_else(|| vendor.name.clone());
    let email = text(input.email)
        .or_else(|| vendor.email.clone())
        .ok_or_else(|| CommandError::Invalid("Mercury needs the vendor's email".into()))?;
    if !email
        .split_once('@')
        .is_some_and(|(l, d)| !l.is_empty() && d.contains('.'))
    {
        return Err(CommandError::Invalid(
            "enter the vendor's email address".into(),
        ));
    }
    let account = input.account_number.trim().to_owned();
    let routing = input.routing_number.trim().to_owned();
    if method != Method::Check {
        if !(4..=17).contains(&account.len()) || !account.chars().all(|c| c.is_ascii_digit()) {
            return Err(CommandError::Invalid(
                "the account number is 4 to 17 digits".into(),
            ));
        }
        if !routing_ok(&routing) {
            return Err(CommandError::Invalid(
                "that isn't a valid 9-digit routing number; check it with the vendor".into(),
            ));
        }
    } else if !account.is_empty() || !routing.is_empty() {
        return Err(CommandError::Invalid(
            "a check needs only the mailing address".into(),
        ));
    }
    let account_type = match input.account_type.as_deref().unwrap_or("business_checking") {
        "business_checking" => "businessChecking",
        "business_savings" => "businessSavings",
        "personal_checking" => "personalChecking",
        "personal_savings" => "personalSavings",
        _ => {
            return Err(CommandError::Invalid(
                "account_type is business_checking, business_savings, personal_checking or personal_savings".into(),
            ));
        }
    };
    let a = &input.address;
    if [&a.address1, &a.city, &a.region, &a.postal_code, &a.country]
        .iter()
        .any(|f| f.trim().is_empty())
    {
        return Err(CommandError::Invalid(
            "the address needs a street, city, state, ZIP and country".into(),
        ));
    }
    let c = live(cx).await?;
    if c.provider != "mercury" {
        return Err(CommandError::Invalid(
            "creating recipients requires Mercury".into(),
        ));
    }
    let client = mercury(cx, &c)?;
    let destination = if method == Method::Check {
        format!("check to {}, {}", a.city.trim(), a.region.trim())
    } else {
        last4(&account)
    };
    let summary = json!({ "vendor_name": vendor.name, "recipient_name": name, "email": email,
        "method": method, "destination": destination, "bank": c.account_name,
        "next": "Mercury creates the recipient; a person accepts it as this vendor's payee before it's paid" });
    if cx.preview {
        return Ok(summary);
    }
    let made = client
        .create_recipient(&payvy_pay::NewRecipient {
            name: &name,
            email: &email,
            method,
            account_number: &account,
            routing_number: &routing,
            account_type,
            address: a,
        })
        .await
        .map_err(|e| match e {
            BankError::Refused(m) | BankError::Unauthorized(m) => {
                CommandError::Bank(format!("Mercury refused the recipient: {m}"))
            }
            BankError::Unclear(m) => CommandError::Bank(format!(
                "{m}. Mercury may have created it: if it's there, propose it with vendor.propose_payee instead of entering the details again"
            )),
        })?;
    // Check what Mercury made against what was typed, by its last four.
    let destination = made.destination(method).map(str::to_owned).ok_or_else(|| {
        CommandError::Bank(format!(
            "Mercury created recipient {} without {} details; look at it in Mercury",
            made.id,
            method.as_str()
        ))
    })?;
    let p = store::insert_payee_proposal(
        &mut cx.tx,
        cx.org,
        &NewPayee {
            vendor_id: vendor.id,
            connection_id: c.id,
            recipient_id: &made.id,
            recipient_name: &made.name,
            method: method.as_str(),
            destination: &destination,
            reason: Some(&format!("Details entered in Payvy by {}", cx.actor.name)),
            proposed_by: &cx.actor.name,
            created_at: cx.now,
        },
    )
    .await?;
    cx.emit(
        "vendor.recipient_created",
        vendor.id,
        json!({ "vendor_id": vendor.id, "recipient_id": made.id, "recipient_name": made.name,
                "method": method, "destination": destination }),
    )
    .await?;
    cx.emit(
        "vendor.payee_proposed",
        vendor.id,
        json!({ "vendor_id": vendor.id, "proposal_id": p.id, "recipient_name": p.recipient_name,
                "method": p.method, "destination": p.destination }),
    )
    .await?;
    Ok(json!({ "proposal": p, "recipient_id": made.id,
               "next": "a person accepts it with vendor.accept_payee, after checking the details with the vendor by phone" }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecidePayee {
    proposal_id: Uuid,
    reason: Option<String>,
}

pub async fn decide_payee(
    cx: &mut Cx<'_>,
    input: DecidePayee,
    accept: bool,
) -> Result<Value, CommandError> {
    store::lock_bank(&mut cx.tx, cx.org).await?;
    let p = store::get_payee(&mut cx.tx, input.proposal_id)
        .await?
        .ok_or(CommandError::NotFound("payee proposal"))?;
    if p.status != "proposed" {
        return Err(CommandError::Conflict(format!(
            "this proposal is {}",
            p.status
        )));
    }
    if accept {
        let live = store::live_paying_connection(&mut cx.tx).await?;
        if live.as_ref().map(|c| c.id) != Some(p.connection_id) {
            return Err(CommandError::Conflict(
                "this was proposed for a bank connection that has since been replaced; propose it again".into(),
            ));
        }
        if let Some(c) = live.filter(|c| c.provider == "ramp") {
            let instrument = p.instrument_id.ok_or_else(|| {
                CommandError::Conflict("Propose a specific Ramp account again".into())
            })?;
            let vendor = Uuid::parse_str(&p.recipient_id)
                .map_err(|_| CommandError::Conflict("Invalid Ramp vendor mapping".into()))?;
            let details = super::ramp::destination(cx, &c, vendor, instrument).await?;
            if details["fingerprint"].as_str() != p.instrument_fingerprint.as_deref() {
                return Err(CommandError::Conflict("The Ramp vendor or account details changed after this proposal; propose them again".into()));
            }
        }
    }
    let previous = store::active_payee(&mut cx.tx, p.vendor_id).await?;
    store::decide_payee(
        &mut cx.tx,
        p.id,
        p.vendor_id,
        accept,
        &cx.actor.name,
        cx.now,
    )
    .await?;
    let kind = if accept {
        "vendor.payee_accepted"
    } else {
        "vendor.payee_declined"
    };
    cx.emit(
        kind,
        p.vendor_id,
        json!({ "vendor_id": p.vendor_id, "proposal_id": p.id, "recipient_name": p.recipient_name,
                "method": p.method, "destination": p.destination,
                "replaced": if accept { previous.map(|x| json!({ "recipient_name": x.recipient_name, "method": x.method, "destination": x.destination })) } else { None },
                "reason": text(input.reason) }),
    )
    .await?;
    Ok(json!({ "payees": store::vendor_payees(&mut cx.tx, p.vendor_id).await? }))
}
