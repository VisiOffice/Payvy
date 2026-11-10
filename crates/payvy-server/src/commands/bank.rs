//! The customer's bank: connecting it, and who each vendor is paid to.
//! Payvy keeps the bank's API token sealed and the recipient's id; account
//! numbers stay in the bank, and people see only the last four digits.

use payvy_pay::{BankError, Mercury, Method, Mode};
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
    store::live_connection(&mut cx.tx).await?.ok_or_else(|| {
        CommandError::Conflict(
            "no bank is connected; a person connects one with bank.connect".into(),
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

pub async fn connect(cx: &mut Cx<'_>, input: Connect) -> Result<Value, CommandError> {
    let bank = cx.bank()?;
    if input.provider != "mercury" {
        return Err(CommandError::Invalid(
            "provider is mercury (Ramp, Brex, Wise and Airwallex are planned)".into(),
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
    let replaced = store::retire_connection(&mut cx.tx, cx.now, &cx.actor.name).await?;
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
    let how = match mode {
        Mode::Request => {
            "Payments wait in Mercury for approval by a Mercury admin other than the token's creator. The token needs the RequestSendMoney scope."
        }
        Mode::Direct => {
            "Mercury sends payments as soon as a person releases them in Payvy. The token must allow this server's IP address."
        }
    };
    Ok(json!({ "connection": c, "how_payments_work": how }))
}

pub async fn get(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    Ok(json!({ "connection": store::live_connection(&mut cx.tx).await? }))
}

pub async fn disconnect(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let c = live(cx).await?;
    store::retire_connection(&mut cx.tx, cx.now, &cx.actor.name).await?;
    cx.emit(
        "bank.disconnected",
        c.id,
        json!({ "connection_id": c.id, "account_name": c.account_name }),
    )
    .await?;
    Ok(json!({
        "disconnected": c.id,
        "note": "No new payments are sent. Payments already sent are still tracked until they finish.",
    }))
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
        let live = store::live_connection(&mut cx.tx).await?;
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
