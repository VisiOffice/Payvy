//! Command dispatch. REST and MCP both land here, so every caller gets the
//! same validation, risk tiers, events and errors. Handlers live one module
//! per area; the registry itself is `payvy_domain::commands::COMMANDS`.

mod auto_pay;
mod bank;
mod bills;
mod documents;
mod email;
mod events;
mod extract;
mod inbox;
mod payee_invites;
mod payments;
mod people;
mod ramp;
mod vendors;
mod workflows;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use payvy_domain::commands::{self, RiskTier};
use payvy_store::{self as store, ActorKind, Tx};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{Actor, AppState};

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("unknown command {0}")]
    Unknown(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    Conflict(String),
    #[error(
        "{0} needs confirmation: call it in preview mode, then commit with the confirm_token it returns"
    )]
    NeedsConfirmation(&'static str),
    #[error("{0}")]
    BadToken(String),
    #[error("only a person can run {0}; an agent can prepare it, and a person signs off")]
    PersonOnly(&'static str),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotConfigured(&'static str),
    /// The customer's bank refused or didn't answer.
    #[error("{0}")]
    Bank(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl CommandError {
    pub fn code(&self) -> &'static str {
        match self {
            CommandError::Unknown(_) => "unknown_command",
            CommandError::Invalid(_) => "invalid_input",
            CommandError::NotFound(_) => "not_found",
            CommandError::Conflict(_) => "conflict",
            CommandError::NeedsConfirmation(_) => "needs_confirmation",
            CommandError::BadToken(_) => "bad_confirm_token",
            CommandError::PersonOnly(_) => "person_only",
            CommandError::Forbidden(_) => "forbidden",
            CommandError::NotConfigured(_) => "not_configured",
            CommandError::Bank(_) => "bank_error",
            CommandError::Internal(_) => "internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            CommandError::Unknown(_) | CommandError::NotFound(_) => StatusCode::NOT_FOUND,
            CommandError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            CommandError::Conflict(_)
            | CommandError::NeedsConfirmation(_)
            | CommandError::BadToken(_) => StatusCode::CONFLICT,
            CommandError::PersonOnly(_) | CommandError::Forbidden(_) => StatusCode::FORBIDDEN,
            CommandError::NotConfigured(_) => StatusCode::SERVICE_UNAVAILABLE,
            CommandError::Bank(_) => StatusCode::BAD_GATEWAY,
            CommandError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Mode {
    /// Run the command and describe the effect, then roll everything back.
    Preview,
    Commit {
        confirm_token: Option<String>,
    },
}

/// Runs one command. A commit returns the resulting state and the events it
/// appended; a write that changes nothing is an error, never a bare success.
pub async fn run(
    state: &AppState,
    actor: &Actor,
    name: &str,
    input: Value,
    mode: Mode,
) -> Result<Value, CommandError> {
    let spec = commands::find(name).ok_or_else(|| CommandError::Unknown(name.to_owned()))?;
    if spec.person_only && actor.kind != ActorKind::Person {
        return Err(CommandError::PersonOnly(spec.name));
    }
    match (spec.tier, &mode) {
        (
            RiskTier::Confirmed,
            Mode::Commit {
                confirm_token: None,
            },
        ) => {
            return Err(CommandError::NeedsConfirmation(spec.name));
        }
        (
            RiskTier::Confirmed,
            Mode::Commit {
                confirm_token: Some(t),
            },
        ) => {
            state
                .confirm
                .check(t, spec.name, &input)
                .map_err(CommandError::BadToken)?;
        }
        _ => {}
    }

    let preview = matches!(mode, Mode::Preview);
    let mut tx = state.store.begin(actor.org_id).await?;
    if let Mode::Commit {
        confirm_token: Some(t),
    } = &mode
        && spec.tier == RiskTier::Confirmed
        && !store::consume_confirm_token(&mut tx, actor.org_id, t, spec.name).await?
    {
        return Err(CommandError::BadToken(
            "confirm_token was already used. Preview again.".into(),
        ));
    }
    let mut cx = Cx {
        tx,
        store: &state.store,
        org: actor.org_id,
        actor,
        now: (state.clock)(),
        events: Vec::new(),
        preview,
        storage: state.storage.as_deref(),
        inbound_domain: state.inbound.as_ref().map(|i| i.domain.as_str()),
        extractor: state.extractor.as_deref(),
        bank: state.bank.as_deref(),
    };
    let result = execute(&mut cx, spec.name, &input).await?;
    let Cx { tx, events, .. } = cx;
    if preview {
        tx.rollback().await.map_err(anyhow::Error::from)?;
    } else {
        tx.commit().await.map_err(anyhow::Error::from)?;
    }

    let mut out = json!({
        "command": spec.name,
        "mode": if preview { "preview" } else { "commit" },
        "result": result,
        "events": events,
    });
    if preview && spec.tier == RiskTier::Confirmed {
        out["confirm_token"] = json!(state.confirm.issue(spec.name, &input));
    }
    Ok(out)
}

async fn execute(cx: &mut Cx<'_>, name: &str, input: &Value) -> Result<Value, CommandError> {
    match name {
        "vendor.create" => vendors::create(cx, parse(input)?).await,
        "vendor.update" => vendors::update(cx, parse(input)?).await,
        "vendor.remove_account" => vendors::remove_account(cx, parse(input)?).await,
        "vendor.list" => vendors::list(cx).await,
        "vendor.get" => vendors::get(cx, parse(input)?).await,
        "vendor.add_account" => vendors::add_account(cx, parse(input)?).await,
        "vendor.invite_payee" => payee_invites::invite(cx, parse(input)?).await,
        "vendor.refresh_payee_invite" => payee_invites::refresh(cx, parse(input)?).await,
        "vendor.propose_payee" => bank::propose_payee(cx, parse(input)?).await,
        "vendor.create_recipient" => bank::create_recipient(cx, parse(input)?).await,
        "vendor.accept_payee" => bank::decide_payee(cx, parse(input)?, true).await,
        "vendor.decline_payee" => bank::decide_payee(cx, parse(input)?, false).await,
        "auto_pay.limits" => auto_pay::limits(cx, parse(input)?).await,
        "auto_pay.lower_limit" => auto_pay::set(cx, parse(input)?, auto_pay::Way::Lower).await,
        "auto_pay.raise_limit" => auto_pay::set(cx, parse(input)?, auto_pay::Way::Raise).await,
        "inbox.get" => inbox::get(cx).await,
        "person.list" => people::list(cx).await,
        "person.add" => people::add(cx, parse(input)?).await,
        "person.remove" => people::remove(cx, parse(input)?).await,
        "bank.authorize" => {
            let _: bank::Accounts = parse(input)?;
            ramp::authorize(cx).await
        }
        "bank.complete_authorization" => ramp::complete(cx, parse(input)?).await,
        "bank.authorization" => ramp::authorization(cx, parse(input)?).await,
        "bank.activate" => ramp::activate(cx, parse(input)?).await,
        "bank.renew" => ramp::renew(cx, parse(input)?).await,
        "bank.connect" => bank::connect(cx, parse(input)?).await,
        "bank.get" => bank::get(cx).await,
        "bank.disconnect" => bank::disconnect(cx, parse(input)?).await,
        "bank.recipients" => bank::recipients(cx, parse(input)?).await,
        "bank.accounts" => bank::accounts(cx, parse(input)?).await,
        "bank.transactions" => bank::transactions(cx, parse(input)?).await,
        "payment.refresh" => payments::refresh(cx, parse(input)?).await,
        "payment.list" => payments::list(cx, parse(input)?).await,
        "workflow.get" => workflows::get(cx, parse(input)?).await,
        "workflow.propose" => workflows::propose(cx, parse(input)?).await,
        "workflow.accept" => workflows::decide(cx, parse(input)?, true).await,
        "workflow.decline" => workflows::decide(cx, parse(input)?, false).await,
        "bill.create" => bills::create(cx, parse(input)?).await,
        "bill.list" => bills::list(cx, parse(input)?).await,
        "bill.get" => bills::get(cx, parse(input)?).await,
        "bill.approve" => bills::approve(cx, parse(input)?).await,
        "bill.reject" => bills::reject(cx, parse(input)?).await,
        "bill.signal" => bills::signal(cx, parse(input)?).await,
        "bill.release" => bills::release(cx, parse(input)?).await,
        "bill.auto_pay" => bills::auto_pay(cx, parse(input)?).await,
        "bill.mark_paid" => bills::mark_paid(cx, parse(input)?).await,
        "bill.void" => bills::void(cx, parse(input)?).await,
        "document.upload" => documents::upload(cx, parse(input)?).await,
        "document.list" => documents::list(cx, parse(input)?).await,
        "document.get" => documents::get(cx, parse(input)?).await,
        "document.trash" => documents::trash(cx, parse(input)?).await,
        "document.extract" => extract::extract(cx, parse(input)?).await,
        "email.address" => email::address(cx, parse(input)?).await,
        "email.new_address" => email::new_address(cx, parse(input)?).await,
        "email.import" => email::import(cx, parse(input)?).await,
        "email.list" => email::list(cx, parse(input)?).await,
        "email.get" => email::get(cx, parse(input)?).await,
        "event.list" => events::list(cx, parse(input)?).await,
        other => Err(CommandError::Unknown(other.to_owned())),
    }
}

/// What a handler works with: one organization-scoped transaction, who is
/// acting, the time, and the events appended so far.
pub(crate) struct Cx<'a> {
    pub tx: Tx,
    pub store: &'a store::Store,
    pub org: Uuid,
    pub actor: &'a Actor,
    pub now: DateTime<Utc>,
    pub events: Vec<String>,
    /// Rolled back at the end. Commands with effects outside the database
    /// (writing to storage) skip them in preview.
    pub preview: bool,
    storage: Option<&'a payvy_storage::Storage>,
    inbound_domain: Option<&'a str>,
    extractor: Option<&'a payvy_extract::Extractor>,
    bank: Option<&'a crate::Bank>,
}

impl<'a> Cx<'a> {
    pub fn storage(&self) -> Result<&'a payvy_storage::Storage, CommandError> {
        self.storage.ok_or(CommandError::NotConfigured(
            "no document storage is configured (set PAYVY_S3_ENDPOINT, PAYVY_S3_BUCKET and its keys)",
        ))
    }

    pub fn inbound_domain(&self) -> Result<&'a str, CommandError> {
        self.inbound_domain.ok_or(CommandError::NotConfigured(
            "email intake isn't configured (set PAYVY_INBOUND_DOMAIN and PAYVY_INBOUND_SECRET)",
        ))
    }

    pub fn extractor(&self) -> Result<&'a payvy_extract::Extractor, CommandError> {
        self.extractor.ok_or(CommandError::NotConfigured(
            "reading documents isn't configured (set PAYVY_EXTRACT_PROVIDER and its key)",
        ))
    }

    pub fn bank(&self) -> Result<&'a crate::Bank, CommandError> {
        self.bank.ok_or(CommandError::NotConfigured(
            "paying through a bank isn't configured (set PAYVY_SECRETS_KEY)",
        ))
    }

    pub async fn emit(
        &mut self,
        kind: &str,
        entity: Uuid,
        payload: Value,
    ) -> Result<(), CommandError> {
        store::append_event(
            &mut self.tx,
            self.org,
            kind,
            Some(entity),
            &self.actor.name,
            &payload,
        )
        .await?;
        self.events.push(kind.to_owned());
        Ok(())
    }
}

fn parse<T: DeserializeOwned>(input: &Value) -> Result<T, CommandError> {
    serde_json::from_value(input.clone()).map_err(|e| CommandError::Invalid(e.to_string()))
}

/// Trimmed, or None when blank.
fn text(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}
