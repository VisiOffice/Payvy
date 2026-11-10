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
    if c.mode == "read_only" {
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
    Ok(json!({ "connection": paying, "connections": connections }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Disconnect {
    provider: Option<String>,
}

pub async fn disconnect(cx: &mut Cx<'_>, input: Disconnect) -> Result<Value, CommandError> {
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
}

pub async fn recipients(cx: &mut Cx<'_>, input: Recipients) -> Result<Value, CommandError> {
    let c = live(cx).await?;
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
    vendor_id: Uuid,
    recipient_id: String,
    method: String,
    reason: Option<String>,
}

pub async fn propose_payee(cx: &mut Cx<'_>, input: ProposePayee) -> Result<Value, CommandError> {
    let method = Method::parse(&input.method).ok_or_else(|| {
        CommandError::Invalid("method is ach, check, domestic_wire or real_time_payment".into())
    })?;
    let vendor = store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let c = live(cx).await?;
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
pub struct DecidePayee {
    proposal_id: Uuid,
    reason: Option<String>,
}

pub async fn decide_payee(
    cx: &mut Cx<'_>,
    input: DecidePayee,
    accept: bool,
) -> Result<Value, CommandError> {
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
