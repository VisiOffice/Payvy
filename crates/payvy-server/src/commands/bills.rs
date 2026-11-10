//! Bills move through their workflow by facts: approvals, signals and a
//! release. Where a bill stands is always `workflow::evaluate` over those
//! facts, so every command checks the evaluated status before it acts.

use std::collections::HashMap;

use chrono::{Duration, NaiveDate};
use payvy_domain::bills::{self as rules, Line, State};
use payvy_domain::workflow::{self, Approval, Facts, Progress, Signal, Status, Workflow};
use payvy_store::{self as store, Bill, InsertBill, NewBill};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, text};

/// The workflow a bill follows: the version it arrived under.
async fn workflow_of(cx: &mut Cx<'_>, bill: &Bill) -> Result<Workflow, CommandError> {
    Ok(match bill.workflow_id {
        Some(id) => store::workflows_by_id(&mut cx.tx, &[id])
            .await?
            .pop()
            .map(|(_, w)| w)
            .ok_or_else(|| anyhow::anyhow!("bill {} points at missing workflow {id}", bill.id))?,
        None => Workflow::builtin_default(),
    })
}

fn evaluate(
    cx: &Cx<'_>,
    bill: &Bill,
    workflow: &Workflow,
    approvals: &[Approval],
    signals: &[Signal],
) -> Progress {
    workflow::evaluate(
        workflow,
        &Facts {
            amount_cents: bill.amount_cents,
            received_at: bill.received_at,
            approvals,
            signals,
            released_at: bill.released_at,
            now: cx.now,
        },
    )
}

/// A locked open bill and where it stands.
async fn open_bill(
    cx: &mut Cx<'_>,
    bill_id: Uuid,
) -> Result<(Bill, Workflow, Progress), CommandError> {
    let bill = store::get_bill(&mut cx.tx, bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    if bill.state != State::Open {
        return Err(CommandError::Conflict(format!(
            "this bill is {}",
            bill.state.as_str()
        )));
    }
    let wf = workflow_of(cx, &bill).await?;
    let (a, s) = store::facts(&mut cx.tx, &[bill.id]).await?;
    let a: Vec<Approval> = a.into_iter().map(|(_, a)| a).collect();
    let s: Vec<Signal> = s.into_iter().map(|(_, s)| s).collect();
    let p = evaluate(cx, &bill, &wf, &a, &s);
    Ok((bill, wf, p))
}

/// Full detail: the bill, lines, progress through its workflow, the facts
/// and its history.
async fn detail(cx: &mut Cx<'_>, bill_id: Uuid) -> Result<Value, CommandError> {
    let bill = store::get_bill(&mut cx.tx, bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    let wf = workflow_of(cx, &bill).await?;
    let (a, s) = store::facts(&mut cx.tx, &[bill.id]).await?;
    let approvals: Vec<Approval> = a.into_iter().map(|(_, a)| a).collect();
    let signals: Vec<Signal> = s.into_iter().map(|(_, s)| s).collect();
    let progress =
        (bill.state == State::Open).then(|| evaluate(cx, &bill, &wf, &approvals, &signals));
    let lines = store::bill_lines(&mut cx.tx, bill.id).await?;
    let history = store::list_events(&mut cx.tx, Some(bill.id), 200).await?;
    Ok(json!({
        "bill": bill,
        "lines": lines,
        "progress": progress,
        "workflow": { "id": bill.workflow_id, "mermaid": wf.mermaid() },
        "approvals": approvals,
        "signals": signals,
        "history": history,
    }))
}

fn status_name(p: &Progress) -> &'static str {
    p.status.name()
}

// ── Create ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    vendor_id: Uuid,
    invoice_number: String,
    invoice_date: NaiveDate,
    due_date: Option<NaiveDate>,
    amount_cents: i64,
    memo: Option<String>,
    external_ref: Option<String>,
    #[serde(default)]
    lines: Vec<Line>,
    #[serde(default)]
    allow_duplicate: bool,
}

pub async fn create(cx: &mut Cx<'_>, input: Create) -> Result<Value, CommandError> {
    let vendor = store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let invoice_number = input.invoice_number.trim().to_owned();
    let due_date = input.due_date.or_else(|| {
        vendor
            .terms_days
            .map(|d| input.invoice_date + Duration::days(i64::from(d)))
    });
    rules::validate(
        input.amount_cents,
        &invoice_number,
        &input.lines,
        input.invoice_date,
        due_date,
    )
    .map_err(|e| CommandError::Invalid(e.to_string()))?;
    let key = rules::invoice_key(&invoice_number);
    let external_ref = text(input.external_ref);
    let memo = text(input.memo);
    let workflow_id = store::effective_workflow(&mut cx.tx, vendor.id)
        .await?
        .map(|w| w.id);
    let new = NewBill {
        vendor_id: vendor.id,
        workflow_id,
        invoice_number: &invoice_number,
        invoice_key: &key,
        invoice_date: input.invoice_date,
        due_date,
        amount_cents: input.amount_cents,
        memo: memo.as_deref(),
        external_ref: external_ref.as_deref(),
        lines: &input.lines,
        created_by: &cx.actor.name,
        received_at: cx.now,
    };
    let id = match store::insert_bill(&mut cx.tx, cx.org, &new, input.allow_duplicate).await? {
        InsertBill::Inserted(id) => id,
        InsertBill::Duplicate(existing) => {
            return Err(CommandError::Conflict(format!(
                "possible duplicate: bill {existing} from {} already has invoice number {invoice_number}. \
                 Pass allow_duplicate if this really is a separate bill.",
                vendor.name
            )));
        }
        InsertBill::ExternalRefTaken(existing) => {
            return Err(CommandError::Conflict(format!(
                "bill {existing} already has this external_ref"
            )));
        }
    };
    cx.emit(
        "bill.created",
        id,
        json!({ "bill_id": id, "vendor_id": vendor.id, "invoice_number": invoice_number,
                "amount_cents": input.amount_cents, "workflow_id": workflow_id, "duplicate_allowed": input.allow_duplicate }),
    )
    .await?;
    detail(cx, id).await
}

// ── Read ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    state: Option<State>,
    status: Option<String>,
    vendor_id: Option<Uuid>,
    #[serde(default)]
    mine: bool,
    limit: Option<usize>,
}

/// Whether a step is waiting on this person: named, or open to anyone.
fn waits_on(status: &Status, who: &str) -> bool {
    match status {
        Status::AwaitingApproval { waiting_on, .. } => {
            waiting_on.is_empty() || waiting_on.iter().any(|w| w == who)
        }
        Status::AwaitingRelease { by } => by.is_empty() || by.iter().any(|w| w == who),
        _ => false,
    }
}

pub async fn list(cx: &mut Cx<'_>, input: List) -> Result<Value, CommandError> {
    let limit = input.limit.unwrap_or(100).clamp(1, 500);
    let derived_filter = input.status.is_some() || input.mine;
    let state = if derived_filter {
        Some(State::Open)
    } else {
        input.state
    };
    if derived_filter && input.state.is_some_and(|s| s != State::Open) {
        return Err(CommandError::Invalid(
            "status and mine only apply to open bills".into(),
        ));
    }
    // Status is derived, so filtering by it reads open bills and evaluates
    // them. Fine at small-business volumes; revisit with an index on a
    // cached status if a customer has many thousands open.
    let fetch = if derived_filter { 5_000 } else { limit as i64 };
    let bills = store::list_bills(&mut cx.tx, state, input.vendor_id, fetch).await?;

    let wf_ids: Vec<Uuid> = bills.iter().filter_map(|b| b.workflow_id).collect();
    let workflows: HashMap<Uuid, Workflow> = store::workflows_by_id(&mut cx.tx, &wf_ids)
        .await?
        .into_iter()
        .collect();
    let open_ids: Vec<Uuid> = bills
        .iter()
        .filter(|b| b.state == State::Open)
        .map(|b| b.id)
        .collect();
    let (approvals, signals) = store::facts(&mut cx.tx, &open_ids).await?;
    let builtin = Workflow::builtin_default();

    let mut out = Vec::new();
    for bill in &bills {
        let progress = (bill.state == State::Open).then(|| {
            let wf = bill
                .workflow_id
                .and_then(|id| workflows.get(&id))
                .unwrap_or(&builtin);
            let a: Vec<Approval> = approvals
                .iter()
                .filter(|(b, _)| *b == bill.id)
                .map(|(_, a)| a.clone())
                .collect();
            let s: Vec<Signal> = signals
                .iter()
                .filter(|(b, _)| *b == bill.id)
                .map(|(_, s)| s.clone())
                .collect();
            evaluate(cx, bill, wf, &a, &s)
        });
        if let Some(want) = &input.status
            && progress.as_ref().map(status_name) != Some(want.as_str())
        {
            continue;
        }
        if input.mine
            && !progress
                .as_ref()
                .is_some_and(|p| waits_on(&p.status, &cx.actor.name))
        {
            continue;
        }
        out.push(json!({ "bill": bill, "progress": progress.map(|p| p.status) }));
        if out.len() == limit {
            break;
        }
    }
    Ok(json!({ "bills": out }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Get {
    bill_id: Uuid,
}

pub async fn get(cx: &mut Cx<'_>, input: Get) -> Result<Value, CommandError> {
    detail(cx, input.bill_id).await
}

// ── Workflow facts ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approve {
    bill_id: Uuid,
    note: Option<String>,
}

pub async fn approve(cx: &mut Cx<'_>, input: Approve) -> Result<Value, CommandError> {
    let (bill, wf, progress) = open_bill(cx, input.bill_id).await?;
    let Status::AwaitingApproval { step_id, .. } = &progress.status else {
        return Err(CommandError::Conflict(format!(
            "this bill isn't waiting for approval; it is {}",
            status_name(&progress)
        )));
    };
    if let Some(workflow::Step {
        kind: workflow::StepKind::Approve { approvers, .. },
        ..
    }) = wf.step(step_id)
        && !approvers.is_empty()
        && !approvers.contains(&cx.actor.name)
    {
        return Err(CommandError::Forbidden(format!(
            "you aren't an approver on step {step_id}; it needs {}",
            approvers.join(" or ")
        )));
    }
    let note = text(input.note);
    if !store::insert_approval(
        &mut cx.tx,
        cx.org,
        bill.id,
        step_id,
        &cx.actor.name,
        note.as_deref(),
        cx.now,
    )
    .await?
    {
        return Err(CommandError::Conflict(format!(
            "you already approved step {step_id}"
        )));
    }
    cx.emit(
        "bill.approved",
        bill.id,
        json!({ "bill_id": bill.id, "step_id": step_id, "note": note }),
    )
    .await?;
    detail(cx, bill.id).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalInput {
    bill_id: Option<Uuid>,
    external_ref: Option<String>,
    signal: String,
    note: Option<String>,
}

pub async fn signal(cx: &mut Cx<'_>, input: SignalInput) -> Result<Value, CommandError> {
    if !workflow::valid_signal(&input.signal) {
        return Err(CommandError::Invalid(
            "signal must be 1-60 characters of a-z, 0-9 or _".into(),
        ));
    }
    let bill_id = match (input.bill_id, text(input.external_ref)) {
        (Some(id), None) => id,
        (None, Some(r)) => store::bill_id_by_external_ref(&mut cx.tx, &r)
            .await?
            .ok_or(CommandError::NotFound("bill"))?,
        _ => {
            return Err(CommandError::Invalid(
                "name the bill by bill_id or external_ref (one of them)".into(),
            ));
        }
    };
    let (bill, wf, progress) = open_bill(cx, bill_id).await?;
    let (_, signals) = store::facts(&mut cx.tx, &[bill.id]).await?;
    if signals.iter().any(|(_, s)| s.key == input.signal) {
        return Err(CommandError::Conflict(format!(
            "{} was already signalled for this bill",
            input.signal
        )));
    }
    let note = text(input.note);
    store::insert_signal(
        &mut cx.tx,
        cx.org,
        bill.id,
        &input.signal,
        &cx.actor.name,
        note.as_deref(),
        cx.now,
    )
    .await?;
    // Say whether anything waits for it: a typo'd signal shouldn't look like progress.
    let used_by = wf.steps.iter().find_map(|s| match &s.kind {
        workflow::StepKind::WaitFor { signal, .. } if *signal == input.signal => Some(s.id.clone()),
        _ => None,
    });
    cx.emit(
        "bill.signalled",
        bill.id,
        json!({ "bill_id": bill.id, "signal": input.signal, "note": note }),
    )
    .await?;
    let mut out = detail(cx, bill.id).await?;
    out["signal_used_by_step"] = json!(used_by);
    out["was"] = json!(status_name(&progress));
    Ok(out)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    bill_id: Uuid,
}

pub async fn release(cx: &mut Cx<'_>, input: Release) -> Result<Value, CommandError> {
    let (bill, _, progress) = open_bill(cx, input.bill_id).await?;
    match &progress.status {
        Status::AwaitingRelease { by } if !by.is_empty() && !by.contains(&cx.actor.name) => {
            return Err(CommandError::Forbidden(format!(
                "this bill is released by {}",
                by.join(" or ")
            )));
        }
        Status::AwaitingRelease { .. } => {}
        Status::ReadyToPay => {
            return Err(CommandError::Conflict(
                "this bill pays automatically; it needs no release".into(),
            ));
        }
        other => {
            return Err(CommandError::Conflict(format!(
                "this bill isn't at its pay step; it is {}",
                other.name()
            )));
        }
    }
    store::release_bill(&mut cx.tx, bill.id, &cx.actor.name, cx.now).await?;
    cx.emit(
        "bill.released",
        bill.id,
        json!({ "bill_id": bill.id, "amount_cents": bill.amount_cents }),
    )
    .await?;
    detail(cx, bill.id).await
}

// ── Closing ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkPaid {
    bill_id: Uuid,
    paid_on: NaiveDate,
    method: String,
    reference: Option<String>,
}

pub async fn mark_paid(cx: &mut Cx<'_>, input: MarkPaid) -> Result<Value, CommandError> {
    if !["ach", "check", "wire", "card", "other"].contains(&input.method.as_str()) {
        return Err(CommandError::Invalid(
            "method is ach, check, wire, card or other".into(),
        ));
    }
    let (bill, _, progress) = open_bill(cx, input.bill_id).await?;
    if !matches!(
        progress.status,
        Status::Released { .. } | Status::ReadyToPay
    ) {
        return Err(CommandError::Conflict(format!(
            "this bill hasn't been cleared for payment; it is {}",
            status_name(&progress)
        )));
    }
    if input.paid_on < bill.invoice_date {
        return Err(CommandError::Invalid(
            "paid_on is before the invoice date".into(),
        ));
    }
    let reference = text(input.reference);
    store::mark_paid(
        &mut cx.tx,
        bill.id,
        input.paid_on,
        &input.method,
        reference.as_deref(),
    )
    .await?;
    cx.emit(
        "bill.paid",
        bill.id,
        json!({ "bill_id": bill.id, "amount_cents": bill.amount_cents, "paid_on": input.paid_on,
                "method": input.method, "reference": reference }),
    )
    .await?;
    detail(cx, bill.id).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Close {
    bill_id: Uuid,
    reason: String,
}

pub async fn reject(cx: &mut Cx<'_>, input: Close) -> Result<Value, CommandError> {
    close(cx, input, State::Rejected, "bill.rejected").await
}

pub async fn void(cx: &mut Cx<'_>, input: Close) -> Result<Value, CommandError> {
    let bill = store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    if bill.state == State::Rejected {
        // A rejected bill can still be voided, freeing its invoice number.
        let reason = text(Some(input.reason))
            .ok_or_else(|| CommandError::Invalid("reason can't be empty".into()))?;
        store::close_bill(&mut cx.tx, bill.id, State::Void, &reason).await?;
        cx.emit(
            "bill.voided",
            bill.id,
            json!({ "bill_id": bill.id, "reason": reason }),
        )
        .await?;
        return detail(cx, bill.id).await;
    }
    close(cx, input, State::Void, "bill.voided").await
}

async fn close(
    cx: &mut Cx<'_>,
    input: Close,
    state: State,
    event: &str,
) -> Result<Value, CommandError> {
    let reason = text(Some(input.reason))
        .ok_or_else(|| CommandError::Invalid("reason can't be empty".into()))?;
    let (bill, _, progress) = open_bill(cx, input.bill_id).await?;
    store::close_bill(&mut cx.tx, bill.id, state, &reason).await?;
    cx.emit(
        event,
        bill.id,
        json!({ "bill_id": bill.id, "reason": reason, "was": status_name(&progress) }),
    )
    .await?;
    detail(cx, bill.id).await
}
