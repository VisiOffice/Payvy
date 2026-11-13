//! Payments: what Payvy asked the bank to do for a bill, and what the bank
//! did. A payment is sent once, under an idempotency key the bank honours,
//! so an unclear answer is resolved by asking again with the same key. The
//! bank's answer moves the bill: sent marks it paid; failed, cancelled or
//! rejected takes back the release so a person can look; an ACH return
//! reopens a paid bill.

use payvy_domain::bills::State;
use payvy_domain::workflow::AutoPayLimit;
use payvy_pay::{BankError, Method, Mode, Observed, Payment, Status};
use payvy_store::{self as store, Bill, NewPayment, Payee, PaymentRow, PaymentUpdate};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, bank};

/// Going or gone: at most one per bill.
pub(super) fn is_live(status: &str) -> bool {
    matches!(
        status,
        "submitting" | "awaiting_bank_approval" | "pending" | "sent"
    )
}

/// Still waiting on the bank.
pub(super) fn is_open(status: &str) -> bool {
    matches!(status, "submitting" | "awaiting_bank_approval" | "pending")
}

/// What a release will send, shown in its preview.
pub(super) fn plan(bill: &Bill, c: &store::BankConnection, p: &Payee) -> Value {
    json!({
        "bank": c.provider, "from_account": c.account_name, "mode": c.mode,
        "to": p.recipient_name, "destination": p.destination, "method": p.method,
        "amount_cents": bill.amount_cents,
        "then": if c.mode == "request" {
            "waits in Mercury until a Mercury admin approves it"
        } else {
            "sent at once"
        },
    })
}

/// Who authorized a payment: a person's release, or a workflow's automatic
/// pay step (which the auto-pay limit caps).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Authority {
    Person,
    Workflow,
}

/// Refuses an automatic payment over the auto-pay limit in force now. Holds
/// the limit lock (shared) for the rest of the transaction, so a limit
/// lowered at this moment waits until this payment is decided.
pub(super) async fn within_auto_pay_limit(
    cx: &mut Cx<'_>,
    bill: &Bill,
) -> Result<(), CommandError> {
    let (company, vendor) =
        store::auto_pay_limits_for_payment(&mut cx.tx, cx.org, bill.vendor_id).await?;
    let limit = AutoPayLimit::effective(company, vendor);
    if !limit.allows(bill.amount_cents) {
        return Err(CommandError::Conflict(format!(
            "{}: a person releases this bill; it isn't paid automatically",
            limit.reason()
        )));
    }
    Ok(())
}

/// Creates the payment and sends it. Called by `bill.release` and
/// `bill.auto_pay` on commit.
pub(super) async fn submit(
    cx: &mut Cx<'_>,
    bill: &Bill,
    c: &store::BankConnection,
    payee: &Payee,
    authority: Authority,
) -> Result<PaymentRow, CommandError> {
    // The last word before the bank is asked: an automatic payment never
    // goes over the limit in force at this moment.
    if authority == Authority::Workflow {
        within_auto_pay_limit(cx, bill).await?;
    }
    let earlier = store::bill_payments(&mut cx.tx, bill.id).await?;
    if let Some(live) = earlier.iter().find(|p| is_live(&p.status)) {
        return Err(CommandError::Conflict(format!(
            "payment {} for this bill is already {}",
            live.id, live.status
        )));
    }
    // The key is the same on a retry of this attempt, and new for the next
    // attempt after a failure.
    let key = format!("payvy-{}-{}", bill.id.simple(), earlier.len() + 1);
    let row = store::insert_payment(
        &mut cx.tx,
        cx.org,
        &NewPayment {
            id: Uuid::new_v4(),
            bill_id: bill.id,
            connection_id: c.id,
            payee_id: payee.id,
            amount_cents: bill.amount_cents,
            method: &payee.method,
            idempotency_key: &key,
            created_at: cx.now,
            created_by: &cx.actor.name,
        },
    )
    .await?;
    cx.emit(
        "payment.created",
        bill.id,
        json!({ "payment_id": row.id, "bill_id": bill.id, "amount_cents": row.amount_cents,
                "to": payee.recipient_name, "destination": payee.destination, "method": payee.method }),
    )
    .await?;
    let outcome = send(cx, c, payee, &row, bill).await?;
    apply(cx, row, bill, outcome).await
}

async fn send(
    cx: &Cx<'_>,
    c: &store::BankConnection,
    payee: &Payee,
    row: &PaymentRow,
    bill: &Bill,
) -> Result<Result<Observed, BankError>, CommandError> {
    let client = bank::mercury(cx, c)?;
    let method = Method::parse(&payee.method)
        .ok_or_else(|| anyhow::anyhow!("payee {} has unknown method {}", payee.id, payee.method))?;
    let mode = Mode::parse(&c.mode)
        .ok_or_else(|| anyhow::anyhow!("connection {} has unknown mode {}", c.id, c.mode))?;
    Ok(client
        .pay(
            mode,
            &Payment {
                account_id: &c.account_id,
                recipient_id: &payee.recipient_id,
                amount_cents: row.amount_cents,
                method,
                idempotency_key: &row.idempotency_key,
                note: &format!("Payvy bill {}", bill.id),
                memo: &format!("Invoice {}", bill.invoice_number),
            },
        )
        .await)
}

/// Records the bank's answer on the payment and moves the bill to match.
async fn apply(
    cx: &mut Cx<'_>,
    row: PaymentRow,
    bill: &Bill,
    outcome: Result<Observed, BankError>,
) -> Result<PaymentRow, CommandError> {
    let observed = match outcome {
        Ok(o) => o,
        // Nothing was created: a definite failure.
        Err(BankError::Refused(m) | BankError::Unauthorized(m)) => Observed {
            status: Status::Failed,
            transaction_id: None,
            request_id: None,
            failure: Some(m),
            link: None,
        },
        // No clear answer: unchanged, asked again later under the same key.
        Err(BankError::Unclear(m)) => {
            tracing::warn!(payment = %row.id, "bank unclear: {m}");
            store::touch_payment(&mut cx.tx, row.id, cx.now).await?;
            return store::get_payment(&mut cx.tx, row.id)
                .await?
                .ok_or(CommandError::NotFound("payment"));
        }
    };
    let before = row.status.clone();
    let after = observed.status.as_str();
    let updated = store::update_payment(
        &mut cx.tx,
        row.id,
        &PaymentUpdate {
            status: after,
            request_id: observed.request_id.as_deref(),
            transaction_id: observed.transaction_id.as_deref(),
            failure: observed.failure.as_deref(),
            link: observed.link.as_deref(),
        },
        cx.now,
    )
    .await?;
    if before == after {
        // A reference can arrive without a status change; the ledger
        // still wants it.
        super::ledger::on_bank_reference(cx, &updated).await;
        return Ok(updated);
    }
    cx.emit(
        &format!("payment.{after}"),
        bill.id,
        json!({ "payment_id": row.id, "bill_id": bill.id, "from": before, "to": after,
                "transaction_id": updated.transaction_id, "failure": updated.failure }),
    )
    .await?;
    match observed.status {
        Status::Sent if bill.state == State::Open => {
            let paid_on = cx.now.date_naive();
            store::mark_paid(
                &mut cx.tx,
                bill.id,
                paid_on,
                &updated.method,
                updated.transaction_id.as_deref(),
            )
            .await?;
            cx.emit(
                "bill.paid",
                bill.id,
                json!({ "bill_id": bill.id, "amount_cents": bill.amount_cents, "paid_on": paid_on,
                        "method": updated.method, "reference": updated.transaction_id,
                        "payment_id": row.id, "via": "bank" }),
            )
            .await?;
            // The ledger follows the money: post (at payment, the bill
            // first) and then the bank reference. Recorded failures
            // never touch the bank facts above.
            super::ledger::on_payment_sent(cx, &updated, bill, paid_on).await;
        }
        Status::Returned if bill.state == State::Paid => {
            store::reopen_returned_bill(&mut cx.tx, bill.id).await?;
            cx.emit(
                "bill.payment_returned",
                bill.id,
                json!({ "bill_id": bill.id, "payment_id": row.id, "failure": updated.failure }),
            )
            .await?;
        }
        s if s.did_not_pay() && bill.state == State::Open => {
            store::unrelease_bill(&mut cx.tx, bill.id).await?;
            cx.emit(
                "bill.payment_failed",
                bill.id,
                json!({ "bill_id": bill.id, "payment_id": row.id, "status": after,
                        "failure": updated.failure,
                        "next": "the bill waits for release again" }),
            )
            .await?;
        }
        _ => {}
    }
    Ok(updated)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refresh {
    payment_id: Uuid,
}

/// Asks the bank where a payment stands and records it. The payment checker
/// runs this for every open payment; anyone may run it to check now.
pub async fn refresh(cx: &mut Cx<'_>, input: Refresh) -> Result<Value, CommandError> {
    let row = store::get_payment(&mut cx.tx, input.payment_id)
        .await?
        .ok_or(CommandError::NotFound("payment"))?;
    let bill = store::get_bill(&mut cx.tx, row.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    if !is_live(&row.status) || cx.preview {
        return Ok(json!({ "payment": row, "changed": false }));
    }
    let c = store::get_connection(&mut cx.tx, row.connection_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("payment {} has no connection", row.id))?;
    let before = row.status.clone();
    let outcome = if row.status == "submitting" {
        let payee = store::get_payee(&mut cx.tx, row.payee_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("payment {} has no payee", row.id))?;
        send(cx, &c, &payee, &row, &bill).await?
    } else {
        bank::mercury(cx, &c)?
            .status(
                &c.account_id,
                row.transaction_id.as_deref(),
                row.request_id.as_deref(),
            )
            .await
    };
    let updated = apply(cx, row, &bill, outcome).await?;
    let changed = updated.status != before;
    Ok(json!({ "payment": updated, "changed": changed }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    #[serde(default)]
    open: bool,
    limit: Option<i64>,
}

pub async fn list(cx: &mut Cx<'_>, input: List) -> Result<Value, CommandError> {
    let limit = input.limit.unwrap_or(100).clamp(1, 500);
    Ok(json!({ "payments": store::list_payments(&mut cx.tx, input.open, limit).await? }))
}
