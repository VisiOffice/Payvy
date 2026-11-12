//! Bills move through their workflow by facts: approvals, signals and a
//! release. Where a bill stands is always `workflow::evaluate` over those
//! facts, so every command checks the evaluated status before it acts.

use std::collections::HashMap;

use chrono::{Duration, NaiveDate};
use payvy_domain::bills::{self as rules, Line, State};
use payvy_domain::duties::{self, Refusal, Rules, Turn};
use payvy_domain::workflow::{
    self, Approval, AutoPayLimit, Facts, Progress, Signal, Status, Workflow,
};
use payvy_store::{self as store, ActorKind, Bill, InsertBill, NewBill};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, documents, extract, payments, text};

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

async fn evaluate(
    cx: &mut Cx<'_>,
    bill: &Bill,
    workflow: &Workflow,
    approvals: &[Approval],
    signals: &[Signal],
    auto_pay_limit: AutoPayLimit,
) -> Result<Progress, CommandError> {
    // A bill payable only by override waits for a person even at an
    // automatic pay step, so it is never ready_to_pay.
    let payable_by_override = super::purchase_orders::payable_by_override(cx, bill).await?;
    Ok(workflow::evaluate(
        workflow,
        &Facts {
            amount_cents: bill.amount_cents,
            received_at: bill.received_at,
            approvals,
            signals,
            released_at: bill.released_at,
            release_by_person: bill.release_by_person,
            payable_by_override,
            auto_pay_limit,
            now: cx.now,
        },
    ))
}

/// What the separation-of-duties rules need besides a bill and its facts:
/// the company's rules, who signs in, and who uploaded each bill's documents.
struct Duties {
    rules: Rules,
    people: Vec<String>,
    uploaders: Vec<(Uuid, String)>,
}

async fn duties(cx: &mut Cx<'_>, bill_ids: &[Uuid]) -> Result<Duties, CommandError> {
    let rules = store::approval_rules(&mut cx.tx).await?;
    let mut people = Vec::new();
    for p in store::people(&mut cx.tx, cx.org).await? {
        if payvy_domain::permissions::Permission::Approve
            .allows(&store::actor_roles(&mut cx.tx, cx.org, &p.actor).await?)
        {
            people.push(p.actor);
        }
    }
    let uploaders = store::bill_uploaders(&mut cx.tx, bill_ids).await?;
    Ok(Duties {
        rules: Rules {
            submitter_cannot_approve: rules.submitter_cannot_approve,
        },
        people,
        uploaders,
    })
}

/// Whether the caller may approve the step this bill waits on, and who
/// else may. None unless it is waiting for approval.
fn turn(
    cx: &Cx<'_>,
    d: &Duties,
    bill: &Bill,
    workflow: &Workflow,
    progress: &Progress,
    approvals: &[Approval],
) -> Option<Turn> {
    let uploaded_by: Vec<String> = d
        .uploaders
        .iter()
        .filter(|(b, _)| *b == bill.id)
        .map(|(_, a)| a.clone())
        .collect();
    let mut turn = duties::turn(
        &duties::Context {
            workflow,
            status: &progress.status,
            amount_cents: bill.amount_cents,
            approvals,
            entered_by: &bill.created_by,
            uploaded_by: &uploaded_by,
            people: &d.people,
            rules: d.rules,
        },
        &cx.actor.name,
        cx.actor.kind == ActorKind::Person,
    );
    if let Some(t) = &mut turn {
        t.others.retain(|actor| d.people.contains(actor));
        if !cx.can("bill.approve") {
            t.can_approve = false;
            t.refusal = Some(Refusal::MissingRole);
            t.message = Some(
                "You need the Approver role to approve bills. Ask an admin to review your access."
                    .into(),
            );
        }
    }
    turn
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
    let limit = store::auto_pay_limits(&mut cx.tx)
        .await?
        .for_vendor(bill.vendor_id);
    let p = evaluate(cx, &bill, &wf, &a, &s, limit).await?;
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
    let limit = store::auto_pay_limits(&mut cx.tx)
        .await?
        .for_vendor(bill.vendor_id);
    let progress = match bill.state {
        State::Open => Some(evaluate(cx, &bill, &wf, &approvals, &signals, limit).await?),
        _ => None,
    };
    let approval = match &progress {
        Some(p) if matches!(p.status, Status::AwaitingApproval { .. }) => {
            let d = duties(cx, &[bill.id]).await?;
            turn(cx, &d, &bill, &wf, p, &approvals)
        }
        _ => None,
    };
    let lines = store::bill_lines(&mut cx.tx, bill.id).await?;
    let purchase_orders = super::purchase_orders::bill_po_view(cx, &bill).await?;
    // Whoever permitted any standing PO exception can't release the bill.
    let release_blocked = match &progress {
        Some(p) if can_release(cx, &p.status) => {
            super::purchase_orders::release_blocked_for(cx, &bill, &cx.actor.name).await?
        }
        _ => false,
    };
    let history = store::list_events(&mut cx.tx, Some(bill.id), 200).await?;
    let documents = store::bill_documents(&mut cx.tx, bill.id).await?;
    let arrangement = match documents.first() {
        Some(doc) => Some(documents::pinned(cx, doc, bill.arrangement_id).await?),
        None => None,
    };
    let payments = store::bill_payments(&mut cx.tx, bill.id).await?;
    let arrivals = store::bill_arrivals::list_arrivals(&mut cx.tx, bill.id).await?;
    let pending_revision = arrivals
        .iter()
        .rev()
        .find(|a| a.decision == "pending")
        .map(|a| {
            json!({ "arrival_id": a.id, "amount_cents": a.amount_cents,
                    "current_amount_cents": bill.amount_cents,
                    "received_at": a.received_at, "received_by": a.received_by })
        });
    let invoice_review = store::bill_invoice_review(&mut cx.tx, bill.id).await?;
    // Versions keep this notice true even if pages were later restored to the
    // same order. The original review and posted lines are never rewritten.
    let invoice_review_pages_changed = invoice_review.as_ref().is_some_and(|r| {
        arrangement
            .as_ref()
            .and_then(|a| a["version"].as_i64())
            .is_some_and(|version| version != i64::from(r.arrangement_version))
    });
    let received_count = arrivals.len();
    Ok(json!({
        "arrivals": arrivals,
        "received_count": received_count,
        "pending_revision": pending_revision,
        "payments": payments,
        "invoice_review": invoice_review,
        "invoice_review_pages_changed": invoice_review_pages_changed,
        "bill": bill,
        "lines": lines,
        "progress": progress,
        "approval": approval,
        "can_release": !release_blocked && progress.as_ref().is_some_and(|p| can_release(cx, &p.status)),
        "workflow": { "id": bill.workflow_id, "mermaid": wf.mermaid() },
        "auto_pay_limit": limit,
        "purchase_orders": purchase_orders,
        "approvals": approvals,
        "signals": signals,
        "documents": documents,
        "arrangement": arrangement,
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
    document_id: Option<Uuid>,
    extraction_id: Option<Uuid>,
    review_id: Option<Uuid>,
    /// Only these pages of the document (the original's page numbers, in
    /// this order): they are split into a new document filed with the bill.
    pages: Option<Vec<u32>>,
    /// The document's pages.version the caller saw.
    expected_version: Option<i32>,
}

pub async fn create(cx: &mut Cx<'_>, mut input: Create) -> Result<Value, CommandError> {
    let review = match input.document_id {
        Some(doc) => {
            // Keep enrollment changes atomic with document-backed bill creation.
            // Manual bills do not participate in enrollment or take its lock.
            store::lock_invoice_settings(&mut cx.tx, cx.org).await?;
            super::invoice_reviews::for_bill(cx, doc, input.review_id).await?
        }
        None if input.review_id.is_some() => {
            return Err(CommandError::Invalid(
                "review_id requires document_id".into(),
            ));
        }
        None => None,
    };
    if let Some(r) = &review {
        let p = &r.payload;
        let v = payvy_domain::invoice_review::validate(p);
        if p.vendor_id.as_deref() != Some(input.vendor_id.to_string().as_str())
            || p.invoice_number.trim() != input.invoice_number.trim()
            || p.invoice_date != Some(input.invoice_date)
            || p.due_date != input.due_date
            || v.total_cents != Some(input.amount_cents)
            || p.memo.trim() != input.memo.as_deref().unwrap_or("").trim()
        {
            return Err(CommandError::Conflict(
                "Bill details differ from the saved review. Save changes first.".into(),
            ));
        }
        let lines = if r.summary_reason.is_some() {
            Vec::new()
        } else {
            v.lines
        };
        if !input.lines.is_empty() && input.lines != lines {
            return Err(CommandError::Invalid(
                "Bill lines must match the saved review".into(),
            ));
        }
        if let Some(pages) = &input.pages {
            let doc = store::get_document(&mut cx.tx, r.document_id)
                .await?
                .ok_or(CommandError::NotFound("document"))?;
            let count = documents::page_count(cx, &doc).await?;
            let a = documents::current_arrangement(cx, doc.id).await?;
            let visible: Vec<_> = documents::arranged(a.as_ref(), count)
                .unwrap_or_default()
                .into_iter()
                .filter(|p| !p.hidden)
                .map(|p| p.page)
                .collect();
            if pages != &visible {
                return Err(CommandError::Invalid(
                    "Split selected pages into a document before reviewing its lines".into(),
                ));
            }
        }
        input.lines = lines;
        input.extraction_id = r.extraction_id;
        input.expected_version = Some(r.arrangement_version);
    }
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
    for line in &input.lines {
        if let Some(q) = &line.quantity
            && payvy_domain::invoice_evidence::quantity_micro(q).is_none()
        {
            return Err(CommandError::Invalid(format!(
                "line {:?} has a quantity that isn't a number: {q:?}",
                line.description
            )));
        }
    }
    let corrected = match input.extraction_id {
        Some(id) => {
            let entered = json!({
                "vendor_id": vendor.id,
                "invoice_number": invoice_number,
                "invoice_date": input.invoice_date,
                "due_date": input.due_date,
                "amount_cents": input.amount_cents,
                "lines": (!input.lines.is_empty()).then_some(&input.lines),
            });
            Some(extract::corrections(cx, id, input.document_id, &entered).await?)
        }
        None => None,
    };
    let key = rules::invoice_key(&invoice_number);
    let external_ref = text(input.external_ref);
    let memo = text(input.memo);
    let workflow_id = store::effective_workflow(&mut cx.tx, vendor.id)
        .await?
        .map(|w| w.id);
    // Pin the vendor's PO policy version in force now, like the workflow:
    // a later policy change never rewrites a bill already in flight.
    let policy_version_id = store::purchase_orders::current_policy(&mut cx.tx, vendor.id)
        .await?
        .map(|p| p.id);
    // Some pages of a document: they become a document of their own first,
    // in this transaction, so a refused bill leaves no split behind.
    let (document_id, split_from) = match input.document_id {
        Some(doc) => documents::for_bill(cx, doc, input.pages.as_deref(), input.expected_version)
            .await
            .map(|(d, from)| (Some(d), from))?,
        None if input.pages.is_some() || input.expected_version.is_some() => {
            return Err(CommandError::Invalid(
                "pages and expected_version need document_id".into(),
            ));
        }
        None => (None, None),
    };
    // A bill from a document pins the page arrangement it was entered from.
    let arrangement_id = match document_id {
        Some(doc) => documents::current_arrangement(cx, doc).await?.map(|a| a.id),
        None => None,
    };
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
        arrangement_id,
        policy_version_id,
    };
    let id = match store::insert_bill(&mut cx.tx, cx.org, &new, input.allow_duplicate).await? {
        InsertBill::Inserted(id) => id,
        InsertBill::Duplicate(existing) => {
            return stack_arrival(
                cx,
                existing,
                input.amount_cents,
                &input.lines,
                input.extraction_id,
                &invoice_number,
                document_id,
                corrected,
            )
            .await;
        }
        InsertBill::ExternalRefTaken(existing) => {
            return Err(CommandError::Conflict(format!(
                "bill {existing} already has this external_ref"
            )));
        }
    };
    if let Some(r) = &review {
        store::link_invoice_review(&mut cx.tx, cx.org, id, r.id).await?;
    }
    cx.emit(
        "bill.created",
        id,
        json!({ "bill_id": id, "vendor_id": vendor.id, "invoice_number": invoice_number,
                "amount_cents": input.amount_cents, "workflow_id": workflow_id, "duplicate_allowed": input.allow_duplicate,
                "extraction_id": input.extraction_id, "corrected": corrected, "review_id": input.review_id,
                "arrangement_id": arrangement_id, "document_id": document_id,
                "split_from": split_from }),
    )
    .await?;
    if let Some(doc) = document_id {
        documents::file_with_bill(cx, doc, id).await?;
    }
    store::bill_arrivals::insert_arrival(
        &mut cx.tx,
        cx.org,
        id,
        document_id,
        input.amount_cents,
        &invoice_number,
        &input.lines,
        &cx.actor.name,
        cx.now,
        "none",
    )
    .await?;
    detail(cx, id).await
}

/// A copy of an invoice that is already a bill: it stacks on that bill
/// instead of becoming a second payable one. Same amount with the same
/// lines is a resend (recorded, nothing changes); a different amount, or
/// the same amount with different lines (quantities at the same total
/// included), on an open bill is a revision a person must accept or
/// reject before the bill can be paid — accepting quantity-checks the
/// new lines, not the old ones. `allow_duplicate` never reaches here: it
/// makes a separate bill, as before, and that choice stays on its
/// `bill.created` event.
#[allow(clippy::too_many_arguments)]
async fn stack_arrival(
    cx: &mut Cx<'_>,
    existing: Uuid,
    amount_cents: i64,
    lines: &[Line],
    extraction_id: Option<Uuid>,
    invoice_number: &str,
    document_id: Option<Uuid>,
    corrected: Option<Vec<String>>,
) -> Result<Value, CommandError> {
    // Locks the bill, so two copies arriving at once stack in order.
    let bill = store::get_bill(&mut cx.tx, existing)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    let current_lines = store::bill_lines(&mut cx.tx, bill.id).await?;
    let kind = if amount_cents == bill.amount_cents && !rules::lines_differ(&current_lines, lines) {
        rules::ArrivalKind::Resend
    } else {
        rules::ArrivalKind::Revision
    };
    let revised = kind == rules::ArrivalKind::Revision;
    let pending = revised && bill.state == State::Open;
    if pending {
        store::bill_arrivals::supersede_pending(&mut cx.tx, bill.id, cx.now).await?;
    }
    if let Some(doc) = document_id {
        documents::file_with_bill(cx, doc, bill.id).await?;
    }
    let arrival_id = store::bill_arrivals::insert_arrival(
        &mut cx.tx,
        cx.org,
        bill.id,
        document_id,
        amount_cents,
        invoice_number,
        lines,
        &cx.actor.name,
        cx.now,
        if pending { "pending" } else { "none" },
    )
    .await?;
    let received_count = store::bill_arrivals::list_arrivals(&mut cx.tx, bill.id)
        .await?
        .len();
    cx.emit(
        "bill.invoice_received",
        bill.id,
        json!({ "bill_id": bill.id, "invoice_number": invoice_number,
                "received_count": received_count, "arrival_id": arrival_id,
                "amount_cents": amount_cents,
                "current_amount_cents": bill.amount_cents,
                "kind": if revised { "revision" } else { "resend" },
                "pending": pending, "document_id": document_id,
                "extraction_id": extraction_id, "corrected": corrected }),
    )
    .await?;
    let mut out = detail(cx, bill.id).await?;
    out["arrival"] = json!({ "received_count": received_count,
        "kind": if revised { "revision" } else { "resend" }, "pending": pending });
    Ok(out)
}

// ── Read ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    state: Option<State>,
    status: Option<String>,
    vendor_id: Option<Uuid>,
    /// Compared by invoice key, as duplicate detection does.
    invoice_number: Option<String>,
    #[serde(default)]
    mine: bool,
    limit: Option<usize>,
    /// With mine: also bills waiting on you that the approval rules keep
    /// you from approving (the inbox shows them as waiting on others).
    #[serde(skip)]
    pub(super) include_blocked: bool,
}

/// Whether a step is waiting on this person: named, or open to anyone.
fn waits_on(status: &Status, who: &str) -> bool {
    match status {
        Status::AwaitingApproval { waiting_on, .. } => {
            waiting_on.is_empty() || waiting_on.iter().any(|w| w == who)
        }
        Status::AwaitingRelease { by, .. } => by.is_empty() || by.iter().any(|w| w == who),
        _ => false,
    }
}

fn can_release(cx: &Cx<'_>, status: &Status) -> bool {
    cx.can("bill.release")
        && matches!(status, Status::AwaitingRelease { .. })
        && waits_on(status, &cx.actor.name)
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
    let key = input.invoice_number.as_deref().map(rules::invoice_key);
    let bills =
        store::list_bills(&mut cx.tx, state, input.vendor_id, key.as_deref(), fetch).await?;

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
    let limits = store::auto_pay_limits(&mut cx.tx).await?;
    let d = duties(cx, &open_ids).await?;
    let all_ids: Vec<Uuid> = bills.iter().map(|b| b.id).collect();
    let arrivals: HashMap<Uuid, (i64, Option<i64>)> =
        store::bill_arrivals::arrival_summaries(&mut cx.tx, &all_ids)
            .await?
            .into_iter()
            .map(|(id, count, pending)| (id, (count, pending)))
            .collect();
    let builtin = Workflow::builtin_default();

    let mut out = Vec::new();
    for bill in &bills {
        let mut approval = None;
        let progress = if bill.state == State::Open {
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
            let p = evaluate(cx, bill, wf, &a, &s, limits.for_vendor(bill.vendor_id)).await?;
            approval = turn(cx, &d, bill, wf, &p, &a);
            Some(p)
        } else {
            None
        };
        // The separation-of-duties rule in the lists: whoever permitted a
        // PO exception can't release that bill, so it never waits on them.
        let overridden_by = match &progress {
            Some(p) if matches!(p.status, Status::AwaitingRelease { .. }) => {
                super::purchase_orders::overridden_by(cx, bill).await?
            }
            _ => None,
        };
        let release_blocked = match &progress {
            Some(p) if matches!(p.status, Status::AwaitingRelease { .. }) => {
                super::purchase_orders::release_blocked_for(cx, bill, &cx.actor.name).await?
            }
            _ => false,
        };
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
        if input.mine
            && progress.as_ref().is_some_and(|p| {
                matches!(p.status, Status::AwaitingRelease { .. }) && !can_release(cx, &p.status)
            })
        {
            continue;
        }
        if input.mine
            && !cx.can("bill.approve")
            && progress
                .as_ref()
                .is_some_and(|p| matches!(p.status, Status::AwaitingApproval { .. }))
        {
            continue;
        }
        if input.mine && !input.include_blocked && approval.as_ref().is_some_and(|t| !t.can_approve)
        {
            continue;
        }
        if input.mine && !input.include_blocked && release_blocked {
            continue;
        }
        let releaseable = !release_blocked
            && progress
                .as_ref()
                .is_some_and(|p| can_release(cx, &p.status));
        let (received_count, pending_amount) = arrivals
            .get(&bill.id)
            .map_or((1, None), |(count, pending)| (*count, *pending));
        let mut item = json!({ "bill": bill, "progress": progress.map(|p| p.status),
            "can_release": releaseable, "overridden_by": overridden_by,
            "received_count": received_count,
            "pending_revision_amount_cents": pending_amount });
        if let Some(t) = approval {
            item["approval"] = json!(t);
        }
        out.push(item);
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

// ── Revised invoices ────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionDecision {
    bill_id: Uuid,
}

/// The revision a person still has to decide for this bill, if any.
async fn pending_revision(
    cx: &mut Cx<'_>,
    bill_id: Uuid,
) -> Result<Option<store::bill_arrivals::Arrival>, CommandError> {
    Ok(store::bill_arrivals::pending_revision(&mut cx.tx, bill_id).await?)
}

/// An undecided revision stops payment: the amount that would leave is
/// exactly what is in dispute. (mark_paid only warns, like a PO exception:
/// it records a payment that already happened.)
async fn ensure_no_pending_revision(cx: &mut Cx<'_>, bill: &Bill) -> Result<(), CommandError> {
    if let Some(a) = pending_revision(cx, bill.id).await? {
        return Err(CommandError::Conflict(format!(
            "invoice {} was revised to {} and the bill still carries {}; \
             accept or reject the revision (bill.revision_accept / bill.revision_reject) before paying",
            bill.invoice_number,
            workflow::dollars(a.amount_cents),
            workflow::dollars(bill.amount_cents)
        )));
    }
    Ok(())
}

async fn decide_revision(
    cx: &mut Cx<'_>,
    bill_id: Uuid,
    accept: bool,
) -> Result<Value, CommandError> {
    let (bill, _, _) = open_bill(cx, bill_id).await?;
    let arrival = pending_revision(cx, bill.id).await?.ok_or_else(|| {
        CommandError::Conflict("no revised invoice is waiting for a decision".into())
    })?;
    if accept {
        // Q8: allocations may not exceed the bill, wherever its amount
        // changes. An override voids itself on a changed amount (its
        // fingerprint no longer matches); allocations are reversed by an
        // explicit record, never silently, so acceptance waits for that.
        let allocated: i64 = store::purchase_orders::allocations_for_bill(&mut cx.tx, bill.id)
            .await?
            .iter()
            .map(|a| a.amount_cents)
            .sum();
        if allocated > arrival.amount_cents {
            return Err(CommandError::Conflict(format!(
                "{} is allocated to purchase orders, over the revised amount {}; \
                 reverse the excess first (po.allocation_reverse), then accept the revision",
                workflow::dollars(allocated),
                workflow::dollars(arrival.amount_cents)
            )));
        }
        store::bill_arrivals::set_bill_amount(
            &mut cx.tx,
            cx.org,
            bill.id,
            arrival.amount_cents,
            &arrival.lines,
        )
        .await?;
        store::bill_arrivals::decide_arrival(
            &mut cx.tx,
            arrival.id,
            "accepted",
            &cx.actor.name,
            cx.now,
        )
        .await?;
        cx.emit(
            "bill.revision_accepted",
            bill.id,
            json!({ "bill_id": bill.id, "arrival_id": arrival.id,
                    "old_amount_cents": bill.amount_cents,
                    "new_amount_cents": arrival.amount_cents }),
        )
        .await?;
    } else {
        store::bill_arrivals::decide_arrival(
            &mut cx.tx,
            arrival.id,
            "rejected",
            &cx.actor.name,
            cx.now,
        )
        .await?;
        cx.emit(
            "bill.revision_rejected",
            bill.id,
            json!({ "bill_id": bill.id, "arrival_id": arrival.id,
                    "rejected_amount_cents": arrival.amount_cents,
                    "amount_cents": bill.amount_cents }),
        )
        .await?;
    }
    detail(cx, bill.id).await
}

pub async fn revision_accept(
    cx: &mut Cx<'_>,
    input: RevisionDecision,
) -> Result<Value, CommandError> {
    decide_revision(cx, input.bill_id, true).await
}

pub async fn revision_reject(
    cx: &mut Cx<'_>,
    input: RevisionDecision,
) -> Result<Value, CommandError> {
    decide_revision(cx, input.bill_id, false).await
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
    // The step's approvers and the separation-of-duties rules. The bill row
    // is locked, so two approvals of one bill can't race past this.
    let (approvals, _) = store::facts(&mut cx.tx, &[bill.id]).await?;
    let approvals: Vec<Approval> = approvals.into_iter().map(|(_, a)| a).collect();
    let d = duties(cx, &[bill.id]).await?;
    if let Some(Turn {
        refusal: Some(refusal),
        message,
        ..
    }) = turn(cx, &d, &bill, &wf, &progress, &approvals)
    {
        let message = message.unwrap_or_default();
        return Err(match refusal {
            Refusal::AlreadyApprovedStep => CommandError::Conflict(message),
            _ => CommandError::Forbidden(message),
        });
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
    /// Release without sending a payment through the connected bank: it
    /// will be paid some other way, then marked paid.
    #[serde(default)]
    pay_elsewhere: bool,
}

pub async fn release(cx: &mut Cx<'_>, input: Release) -> Result<Value, CommandError> {
    let (bill, _, progress) = open_bill(cx, input.bill_id).await?;
    match &progress.status {
        Status::AwaitingRelease { by, .. } if !by.is_empty() && !by.contains(&cx.actor.name) => {
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
    ensure_no_pending_revision(cx, &bill).await?;
    super::purchase_orders::ensure_payable(cx, &bill).await?;
    super::purchase_orders::ensure_not_overrider(cx, &bill).await?;
    // With a paying bank connected, a release pays through it, to the payee a
    // person accepted for this vendor. With no paying bank — Rho connected
    // or none — a release records that it will be paid elsewhere, exactly
    // as it did before Rho existed.
    let connection = store::live_paying_connection(&mut cx.tx).await?;
    let pay = match (&connection, input.pay_elsewhere) {
        (Some(c), false) => {
            if c.provider == "ramp" {
                return Err(CommandError::Conflict("Ramp payments are not enabled. Pay outside Payvy and use pay_elsewhere, or reconnect Mercury.".into()));
            }
            let vendor = store::get_vendor(&mut cx.tx, bill.vendor_id)
                .await?
                .ok_or(CommandError::NotFound("vendor"))?;
            let payee = store::active_payee(&mut cx.tx, bill.vendor_id)
                .await?
                .filter(|p| p.connection_id == c.id)
                .ok_or_else(|| {
                    CommandError::Conflict(format!(
                        "{} has no payee at {} yet: propose one with vendor.propose_payee and have a person \
                         accept it, or release with pay_elsewhere: true to pay outside Payvy",
                        vendor.name, c.account_name
                    ))
                })?;
            Some((c.clone(), payee))
        }
        _ => None,
    };
    store::release_bill(&mut cx.tx, bill.id, &cx.actor.name, cx.now).await?;
    let plan = pay.as_ref().map(|(c, p)| payments::plan(&bill, c, p));
    cx.emit(
        "bill.released",
        bill.id,
        json!({ "bill_id": bill.id, "amount_cents": bill.amount_cents, "payment": plan }),
    )
    .await?;
    if let Some((c, payee)) = &pay
        && !cx.preview
    {
        // The bank is asked only on commit; the preview shows the plan.
        payments::submit(cx, &bill, c, payee, payments::Authority::Person).await?;
    }
    let mut out = detail(cx, bill.id).await?;
    out["payment_plan"] = json!(plan);
    if pay.is_none() {
        out["next"] = json!("pay it outside Payvy, then record it with bill.mark_paid");
    }
    Ok(out)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoPay {
    bill_id: Uuid,
}

/// Pays a bill whose workflow pays automatically, now that it has reached
/// that step. The authority is the workflow a person accepted, not the
/// caller, so anyone may run it (the payment worker does, as `workflow`);
/// it pays only through Mercury, only to the vendor's accepted payee, under
/// the same idempotency rule as a release. A payment that doesn't go sends
/// the bill to a person (`release_by_person`); it is never retried here.
/// A bill over the auto-pay limit is never ready to pay (it waits for a
/// release), and the limit is checked again, under its lock, as the payment
/// is sent.
pub async fn auto_pay(cx: &mut Cx<'_>, input: AutoPay) -> Result<Value, CommandError> {
    let (bill, _, progress) = open_bill(cx, input.bill_id).await?;
    if progress.status != Status::ReadyToPay {
        let why = match &progress.status {
            Status::AwaitingRelease {
                reason: Some(r), ..
            } => format!(" ({r}; a person releases it)"),
            _ => String::new(),
        };
        return Err(CommandError::Conflict(format!(
            "this bill doesn't pay automatically now; it is {}{why}",
            status_name(&progress)
        )));
    }
    // Checked here as well as at sending, so a preview says it too.
    payments::within_auto_pay_limit(cx, &bill).await?;
    ensure_no_pending_revision(cx, &bill).await?;
    super::purchase_orders::ensure_payable(cx, &bill).await?;
    let c = store::live_paying_connection(&mut cx.tx)
        .await?
        .filter(|c| c.provider == "mercury")
        .ok_or_else(|| {
            CommandError::Conflict(
                "no paying bank is connected: pay it outside Payvy, then record it with bill.mark_paid"
                    .into(),
            )
        })?;
    let payee = store::active_payee(&mut cx.tx, bill.vendor_id)
        .await?
        .filter(|p| p.connection_id == c.id)
        .ok_or_else(|| {
            CommandError::Conflict(format!(
                "{} has no accepted payee at {} yet, so it can't be paid automatically",
                bill.vendor_name, c.account_name
            ))
        })?;
    store::release_bill(&mut cx.tx, bill.id, "workflow", cx.now).await?;
    let plan = payments::plan(&bill, &c, &payee);
    cx.emit(
        "bill.auto_released",
        bill.id,
        json!({ "bill_id": bill.id, "amount_cents": bill.amount_cents,
                "workflow_id": bill.workflow_id, "payment": plan }),
    )
    .await?;
    if !cx.preview {
        payments::submit(cx, &bill, &c, &payee, payments::Authority::Workflow).await?;
    }
    let mut out = detail(cx, bill.id).await?;
    out["payment_plan"] = plan;
    Ok(out)
}

/// Refuses while the bank is still working on a payment for this bill.
async fn no_open_payment(cx: &mut Cx<'_>, bill_id: Uuid) -> Result<(), CommandError> {
    let payments = store::bill_payments(&mut cx.tx, bill_id).await?;
    if let Some(p) = payments.iter().find(|p| payments::is_open(&p.status)) {
        return Err(CommandError::Conflict(format!(
            "payment {} for this bill is {} at the bank; wait for it, or cancel it in the bank",
            p.id, p.status
        )));
    }
    Ok(())
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
    no_open_payment(cx, bill.id).await?;
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
    // mark_paid records a payment that already happened outside Payvy, so a
    // PO exception warns rather than refusing to record reality.
    let po_warning = super::purchase_orders::bill_exception(cx, &bill)
        .await?
        .map(|e| format!("paid while a PO exception stood: {} ({})", e.kind, e.detail));
    let revision_warning = pending_revision(cx, bill.id).await?.map(|a| {
        format!(
            "paid while a revised invoice ({}) was undecided; the bill carries {}",
            workflow::dollars(a.amount_cents),
            workflow::dollars(bill.amount_cents)
        )
    });
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
    let mut out = detail(cx, bill.id).await?;
    out["po_warning"] = json!(po_warning);
    out["revision_warning"] = json!(revision_warning);
    Ok(out)
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
        super::purchase_orders::release_bill_allocations(cx, bill.id, "bill voided").await?;
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
    no_open_payment(cx, bill.id).await?;
    store::close_bill(&mut cx.tx, bill.id, state, &reason).await?;
    let release_reason = format!(
        "bill {}",
        if state == State::Void {
            "voided"
        } else {
            "rejected"
        }
    );
    super::purchase_orders::release_bill_allocations(cx, bill.id, &release_reason).await?;
    cx.emit(
        event,
        bill.id,
        json!({ "bill_id": bill.id, "reason": reason, "was": status_name(&progress) }),
    )
    .await?;
    detail(cx, bill.id).await
}

/// Notification candidates use the same evaluated, pinned workflow and pure
/// approval rules as an interactive approval. Never infer recipients from a role alone.
pub(super) async fn notification_targets(
    cx: &mut Cx<'_>,
    bill_id: Uuid,
) -> Result<(Bill, String, String, Vec<store::Person>), CommandError> {
    use payvy_domain::permissions::Permission;
    let bill = store::get_bill(&mut cx.tx, bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    if bill.state != State::Open {
        return Ok((bill, String::new(), String::new(), vec![]));
    }
    let wf = workflow_of(cx, &bill).await?;
    let (a, s) = store::facts(&mut cx.tx, &[bill.id]).await?;
    let approvals: Vec<_> = a.into_iter().map(|(_, a)| a).collect();
    let signals: Vec<_> = s.into_iter().map(|(_, s)| s).collect();
    let limit = store::auto_pay_limits(&mut cx.tx)
        .await?
        .for_vendor(bill.vendor_id);
    let progress = evaluate(cx, &bill, &wf, &approvals, &signals, limit).await?;
    let action = match progress.status {
        Status::AwaitingApproval { .. } => "approve",
        Status::AwaitingRelease { .. } => "release",
        _ => return Ok((bill, String::new(), String::new(), vec![])),
    };
    let step = progress.active_step().unwrap_or_default().to_owned();
    let d = duties(cx, &[bill.id]).await?;
    let uploaded_by: Vec<_> = d.uploaders.iter().map(|(_, a)| a.clone()).collect();
    let overriders = super::purchase_orders::overriders(cx, &bill).await?;
    let mut recipients = vec![];
    for person in store::people(&mut cx.tx, cx.org).await? {
        let roles = store::actor_roles(&mut cx.tx, cx.org, &person.actor).await?;
        let eligible = match &progress.status {
            Status::AwaitingApproval { .. } => {
                Permission::Approve.allows(&roles)
                    && duties::turn(
                        &duties::Context {
                            workflow: &wf,
                            status: &progress.status,
                            amount_cents: bill.amount_cents,
                            approvals: &approvals,
                            entered_by: &bill.created_by,
                            uploaded_by: &uploaded_by,
                            people: &d.people,
                            rules: d.rules,
                        },
                        &person.actor,
                        true,
                    )
                    .is_some_and(|t| t.can_approve)
            }
            Status::AwaitingRelease { by, .. } => {
                Permission::Pay.allows(&roles)
                    && (by.is_empty() || by.contains(&person.actor))
                    && !overriders
                        .iter()
                        .any(|by| duties::release_refusal(Some(by), &person.actor).is_some())
            }
            _ => false,
        };
        if eligible {
            recipients.push(person);
        }
    }
    Ok((bill, step, action.to_owned(), recipients))
}
