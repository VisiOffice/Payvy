//! Command dispatch. REST and MCP both land here, so every caller gets the
//! same validation, risk tiers, events and errors. Handlers live one module
//! per area; the registry itself is `payvy_domain::commands::COMMANDS`.

mod approval_rules;
mod auto_pay;
mod bank;
mod bill_batches;
mod bills;
mod documents;
mod email;
mod events;
mod extract;
mod inbox;
mod invoice_reviews;
mod ledger;
mod notifications;
mod org;
mod payee_invites;
mod payments;
mod people;
mod purchase_orders;
mod ramp;
mod ramp_health;
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
    /// A ledger refusal with the stable code a person acts on. The link row
    /// is kept: `run` commits this error instead of rolling it back.
    #[error("{detail}")]
    Ledger { code: &'static str, detail: String },
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
            CommandError::Ledger { code, .. } => code,
            CommandError::Internal(_) => "internal",
        }
    }

    /// The sync log is the point of a ledger refusal, so its row is committed.
    fn persists(&self) -> bool {
        matches!(self, CommandError::Ledger { .. })
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
            CommandError::Ledger { code, .. } => match *code {
                "conflict" => StatusCode::CONFLICT,
                "needs_auth" | "concurrency" | "timeout" => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            },
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
    let preview = matches!(mode, Mode::Preview);
    let mut tx = state.store.begin(actor.org_id).await?;
    if matches!(name, "person.add" | "person.remove" | "person.set_roles") {
        store::lock_roles(&mut tx, actor.org_id).await?;
    }
    let roles = crate::permissions::roles(&mut tx, actor).await?;
    if !crate::permissions::allowed(actor, &roles, spec) {
        return Err(CommandError::Forbidden(format!(
            "{} requires the {} role",
            spec.name,
            spec.permission.label()
        )));
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
        roles,
        store: &state.store,
        org: actor.org_id,
        actor,
        now: (state.clock)(),
        clock: &state.clock,
        events: Vec::new(),
        extraction_claim: None,
        preview,
        storage: state.storage.as_deref(),
        inbound_domain: state.inbound.as_ref().map(|i| i.domain.as_str()),
        extractor: state.extractor.as_deref(),
        bank: state.bank.as_deref(),
        auth: state.auth.as_deref(),
        ledger: state.ledger.as_deref(),
    };
    let result = execute(&mut cx, spec.name, &input).await;
    let Cx {
        tx,
        events,
        extraction_claim,
        ..
    } = cx;
    let result = match result {
        Ok(value) => value,
        Err(error) => {
            // A ledger refusal is the sync log: the link row and its event
            // stay. Preview still rolls back. Any other error never happened.
            let keep = !preview && error.persists();
            let finished = if keep {
                tx.commit().await
            } else {
                tx.rollback().await
            };
            if let Err(db) = finished {
                cleanup_extraction_claim(state, actor.org_id, extraction_claim).await;
                return Err(anyhow::Error::from(db).into());
            }
            cleanup_extraction_claim(state, actor.org_id, extraction_claim).await;
            return Err(error);
        }
    };
    let finished = if preview {
        tx.rollback().await
    } else {
        tx.commit().await
    };
    if let Err(error) = finished {
        cleanup_extraction_claim(state, actor.org_id, extraction_claim).await;
        return Err(anyhow::Error::from(error).into());
    }
    // A new batch: wake the worker rather than wait for its next pass.
    if !preview && spec.name == "document.create_bills" {
        state.bill_batches.notify_one();
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

/// Best effort only: the expiring lease remains the fallback for a DB outage,
/// process crash or task cancellation. Token matching never clears a newer read.
async fn cleanup_extraction_claim(state: &AppState, org: Uuid, claim: Option<(Uuid, Uuid)>) {
    let Some((document, token)) = claim else {
        return;
    };
    let cleanup = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut tx = state.store.begin(org).await?;
        store::finish_extraction_read(&mut tx, document, token).await?;
        tx.commit().await?;
        Ok::<(), anyhow::Error>(())
    })
    .await;
    if !matches!(cleanup, Ok(Ok(()))) {
        tracing::warn!(%document, "Could not release failed extraction claim; its expiry remains in force");
    }
}

async fn execute(cx: &mut Cx<'_>, name: &str, input: &Value) -> Result<Value, CommandError> {
    match name {
        "invoice_review.settings" => invoice_reviews::settings(cx).await,
        "invoice_review.configure" => invoice_reviews::configure(cx, parse(input)?).await,
        "document.review_get" => invoice_reviews::get(cx, parse(input)?).await,
        "document.review_save" => invoice_reviews::save(cx, parse(input)?).await,
        "document.review_preview" => invoice_reviews::preview(cx, parse(input)?).await,
        "document.review_accept_summary" => {
            invoice_reviews::accept_summary(cx, parse(input)?).await
        }
        "notification.settings" => notifications::settings(cx, parse(input)?).await,
        "notification.configure" => notifications::configure(cx, parse(input)?).await,
        "notification.list" => notifications::list(cx, parse(input)?).await,
        "notification.queue" => notifications::queue(cx, parse(input)?).await,
        "notification.deliver" => notifications::deliver(cx, parse(input)?).await,
        "vendor.create" => vendors::create(cx, parse(input)?).await,
        "vendor.update" => vendors::update(cx, parse(input)?).await,
        "vendor.link_external" => vendors::link_external(cx, parse(input)?).await,
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
        "approval_rules.get" => approval_rules::get(cx).await,
        "approval_rules.set" => approval_rules::set(cx, parse(input)?).await,
        "person.list" => people::list(cx).await,
        "person.add" => people::add(cx, parse(input)?).await,
        "person.set_roles" => people::set_roles(cx, parse(input)?).await,
        "person.remove" => people::remove(cx, parse(input)?).await,
        "org.get" => org::get(cx).await,
        "org.update" => org::update(cx, parse(input)?).await,
        "bank.authorize" => {
            let _: bank::Accounts = parse(input)?;
            ramp::authorize(cx).await
        }
        "bank.complete_authorization" => ramp::complete(cx, parse(input)?).await,
        "bank.authorization" => ramp::authorization(cx, parse(input)?).await,
        "bank.activate" => ramp::activate(cx, parse(input)?).await,
        "bank.renew" => ramp::renew(cx, parse(input)?).await,
        "bank.maintain" => ramp_health::maintain(cx, parse(input)?, false).await,
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
        "bill.revision_accept" => bills::revision_accept(cx, parse(input)?).await,
        "bill.revision_reject" => bills::revision_reject(cx, parse(input)?).await,
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
        "document.arrange" => documents::arrange(cx, parse(input)?).await,
        "document.split" => documents::split(cx, parse(input)?).await,
        "document.create_bills" => bill_batches::create(cx, parse(input)?).await,
        "document.unsplit" => documents::unsplit(cx, parse(input)?).await,
        "email.address" => email::address(cx, parse(input)?).await,
        "email.new_address" => email::new_address(cx, parse(input)?).await,
        "email.import" => email::import(cx, parse(input)?).await,
        "email.list" => email::list(cx, parse(input)?).await,
        "email.get" => email::get(cx, parse(input)?).await,
        "po.settings" => purchase_orders::settings(cx, parse(input)?).await,
        "po.configure" => purchase_orders::configure(cx, parse(input)?).await,
        "po.create" => purchase_orders::create(cx, parse(input)?).await,
        "po.update" => purchase_orders::update(cx, parse(input)?).await,
        "po.activate" => purchase_orders::activate(cx, parse(input)?).await,
        "po.close" => purchase_orders::close(cx, parse(input)?, "closed").await,
        "po.cancel" => purchase_orders::close(cx, parse(input)?, "canceled").await,
        "po.suspend" => purchase_orders::suspend(cx, parse(input)?).await,
        "po.reactivate" => purchase_orders::reactivate(cx, parse(input)?).await,
        "po.get" => purchase_orders::get(cx, parse(input)?).await,
        "po.list" => purchase_orders::list(cx, parse(input)?).await,
        "po.import" => purchase_orders::import(cx, parse(input)?).await,
        "po.upsert" => purchase_orders::upsert(cx, parse(input)?).await,
        "po.source_authorize" => purchase_orders::source_authorize(cx, parse(input)?).await,
        "po.source_revoke" => purchase_orders::source_revoke(cx, parse(input)?).await,
        "po.vendor_settings" => purchase_orders::vendor_settings(cx, parse(input)?).await,
        "po.set_vendor_settings" => purchase_orders::set_vendor_settings(cx, parse(input)?).await,
        "po.policy_get" => purchase_orders::policy_get(cx, parse(input)?).await,
        "po.policy_set" => purchase_orders::policy_set(cx, parse(input)?).await,
        "po.match_preview" => purchase_orders::match_preview(cx, parse(input)?).await,
        "po.match_apply" => purchase_orders::match_apply(cx, parse(input)?).await,
        "po.override" => purchase_orders::override_exception(cx, parse(input)?).await,
        "po.allocation_accept" => purchase_orders::allocation_accept(cx, parse(input)?).await,
        "po.allocation_reverse" => purchase_orders::allocation_reverse(cx, parse(input)?).await,
        "event.list" => events::list(cx, parse(input)?).await,
        "ledger.post_bill" => ledger::post_bill(cx, parse(input)?).await,
        "ledger.post_payment" => ledger::post_payment(cx, parse(input)?).await,
        "ledger.void" => ledger::void(cx, parse(input)?).await,
        other => Err(CommandError::Unknown(other.to_owned())),
    }
}

/// What a handler works with: one organization-scoped transaction, who is
/// acting, the time, and the events appended so far.
pub(crate) struct Cx<'a> {
    /// A committed extraction lease to clean up if the command/commit fails.
    extraction_claim: Option<(Uuid, Uuid)>,
    pub tx: Tx,
    pub roles: Vec<payvy_domain::permissions::Role>,
    pub store: &'a store::Store,
    pub org: Uuid,
    pub actor: &'a Actor,
    pub now: DateTime<Utc>,
    /// Long provider calls can re-read the injected clock before scheduling retries.
    pub clock: &'a crate::Clock,
    pub events: Vec<String>,
    /// Rolled back at the end. Commands with effects outside the database
    /// (writing to storage) skip them in preview.
    pub preview: bool,
    storage: Option<&'a payvy_storage::Storage>,
    inbound_domain: Option<&'a str>,
    extractor: Option<&'a payvy_extract::Extractor>,
    bank: Option<&'a crate::Bank>,
    auth: Option<&'a crate::auth::Auth>,
    ledger: Option<&'a (dyn payvy_ledger::Ledger + Send + Sync)>,
}

impl<'a> Cx<'a> {
    pub fn can(&self, name: &str) -> bool {
        commands::find(name)
            .is_some_and(|spec| crate::permissions::allowed(self.actor, &self.roles, spec))
    }

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

    pub fn ledger(&self) -> Result<&'a (dyn payvy_ledger::Ledger + Send + Sync), CommandError> {
        self.ledger
            .ok_or(CommandError::NotConfigured("no ledger is configured"))
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
