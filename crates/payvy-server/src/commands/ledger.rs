//! Posting to the company's ledger. The connection is inserted with a sealed
//! token; these commands never read that token and never put it in a result
//! or an event. Phase one calls whatever [`payvy_ledger::Ledger`] the server
//! was given. Production leaves it unset.

use payvy_domain::bills::State;
use payvy_ledger::{
    self, Books, DraftLine, LedgerError, LinkStatus, LinkView, Plan, PostedOutcome, PrepareError,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx};
use payvy_store::{self as store};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillId {
    bill_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaymentId {
    payment_id: Uuid,
}

pub async fn post_bill(cx: &mut Cx<'_>, input: BillId) -> Result<Value, CommandError> {
    require_ledger(cx)?;
    let connection = connection(cx).await?;
    let bill = store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    if matches!(bill.state, State::Rejected | State::Void) {
        return Err(CommandError::Invalid(
            "a rejected or void bill is not posted to the ledger".into(),
        ));
    }
    let vendor = vendor_external(
        store::ledger_link(&mut cx.tx, connection.id, "vendor", bill.vendor_id).await?,
    );
    let records = store::ledger_records(&mut cx.tx, connection.id).await?;
    let books = books_for(&connection, &records, &vendor);
    let lines = store::bill_lines(&mut cx.tx, bill.id).await?;
    let draft = payvy_ledger::BillDraft {
        bill_id: bill.id,
        vendor_external_id: vendor,
        invoice_number: bill.invoice_number.clone(),
        invoice_date: bill.invoice_date,
        due_date: bill.due_date,
        memo: bill.memo.unwrap_or_default(),
        amount_cents: bill.amount_cents,
        lines: lines
            .into_iter()
            .map(|line| DraftLine {
                account_id: line.account.unwrap_or_default(),
                amount_cents: line.amount_cents,
                memo: line.description,
                department_id: None,
                class_id: None,
                location_id: None,
            })
            .collect(),
    };
    let existing = store::ledger_link(&mut cx.tx, connection.id, "bill", bill.id).await?;
    let view = existing.as_ref().map(view_of).transpose()?;
    let key = payvy_ledger::bill_key(bill.id);
    if cx.preview {
        return plan_value(payvy_ledger::plan_bill(&books, &draft, view.as_ref()));
    }
    let submitted = {
        let ledger = cx.ledger()?;
        payvy_ledger::submit_bill(ledger, &books, &draft, view.as_ref()).await
    };
    let outcome = match submitted {
        Ok(outcome) => outcome,
        Err(error) => return record_failure(cx, connection.id, "bill", bill.id, &key, error).await,
    };
    if !outcome.replay {
        store::put_ledger_link(
            &mut cx.tx,
            cx.org,
            &store::PutLedgerLink {
                connection_id: connection.id,
                entity: "bill",
                payvy_id: bill.id,
                external_id: Some(&outcome.posted.external_id),
                external_key: &outcome.posted.external_key,
                status: "posted",
                error_code: None,
                error_detail: None,
                request_hash: Some(&outcome.hash),
                posted_at: Some(cx.now),
            },
        )
        .await?;
    }
    cx.emit(
        "ledger.bill_posted",
        bill.id,
        json!({
            "bill_id": bill.id,
            "external_id": outcome.posted.external_id,
            "external_key": outcome.posted.external_key,
            "replay": outcome.replay,
        }),
    )
    .await?;
    Ok(posted_json(&outcome))
}

pub async fn post_payment(cx: &mut Cx<'_>, input: PaymentId) -> Result<Value, CommandError> {
    require_ledger(cx)?;
    let connection = connection(cx).await?;
    let payment = store::get_payment(&mut cx.tx, input.payment_id)
        .await?
        .ok_or(CommandError::NotFound("payment"))?;
    if payment.status != "sent" {
        return Err(CommandError::Invalid(
            "only a sent payment is posted to the ledger".into(),
        ));
    }
    let bill = store::get_bill(&mut cx.tx, payment.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    let vendor = vendor_external(
        store::ledger_link(&mut cx.tx, connection.id, "vendor", bill.vendor_id).await?,
    );
    let records = store::ledger_records(&mut cx.tx, connection.id).await?;
    let books = books_for(&connection, &records, &vendor);
    let bill_link = store::ledger_link(&mut cx.tx, connection.id, "bill", bill.id).await?;
    let bill_external_id = bill_link
        .filter(|link| link.status == "posted")
        .and_then(|link| link.external_id)
        .unwrap_or_default();
    let draft = payvy_ledger::PaymentDraft {
        payment_id: payment.id,
        amount_cents: payment.amount_cents,
        tran_date: payment.updated_at.date_naive(),
        memo: match &payment.transaction_id {
            Some(reference) => format!("{} {reference}", payment.method),
            None => payment.method.clone(),
        },
        bank_account_id: mapped_account(&connection.bank_account_map, payment.connection_id),
        bill_external_id,
        vendor_external_id: vendor,
    };
    let existing = store::ledger_link(&mut cx.tx, connection.id, "payment", payment.id).await?;
    let view = existing.as_ref().map(view_of).transpose()?;
    let key = payvy_ledger::payment_key(payment.id);
    if cx.preview {
        return plan_value(payvy_ledger::plan_payment(&books, &draft, view.as_ref()));
    }
    let submitted = {
        let ledger = cx.ledger()?;
        payvy_ledger::submit_payment(ledger, &books, &draft, view.as_ref()).await
    };
    let outcome = match submitted {
        Ok(outcome) => outcome,
        Err(error) => {
            return record_failure(cx, connection.id, "payment", payment.id, &key, error).await;
        }
    };
    if !outcome.replay {
        store::put_ledger_link(
            &mut cx.tx,
            cx.org,
            &store::PutLedgerLink {
                connection_id: connection.id,
                entity: "payment",
                payvy_id: payment.id,
                external_id: Some(&outcome.posted.external_id),
                external_key: &outcome.posted.external_key,
                status: "posted",
                error_code: None,
                error_detail: None,
                request_hash: Some(&outcome.hash),
                posted_at: Some(cx.now),
            },
        )
        .await?;
    }
    cx.emit(
        "ledger.payment_posted",
        payment.id,
        json!({
            "payment_id": payment.id,
            "bill_id": payment.bill_id,
            "external_id": outcome.posted.external_id,
            "external_key": outcome.posted.external_key,
            "replay": outcome.replay,
        }),
    )
    .await?;
    Ok(posted_json(&outcome))
}

pub async fn void(cx: &mut Cx<'_>, input: BillId) -> Result<Value, CommandError> {
    require_ledger(cx)?;
    let connection = connection(cx).await?;
    let bill = store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    let bill_link = store::ledger_link(&mut cx.tx, connection.id, "bill", bill.id).await?;
    let bill_view = bill_link.as_ref().map(view_of).transpose()?;
    let payments = store::bill_payments(&mut cx.tx, bill.id).await?;
    let mut posted_payment: Option<(Uuid, store::LedgerLink)> = None;
    for payment in &payments {
        let Some(link) =
            store::ledger_link(&mut cx.tx, connection.id, "payment", payment.id).await?
        else {
            continue;
        };
        if link.status == "posted" {
            posted_payment = Some((payment.id, link));
            break;
        }
    }
    match bill_view.as_ref().map(|link| link.status) {
        Some(LinkStatus::Voided) => Err(CommandError::Ledger {
            code: "conflict",
            detail: "This was voided in the ledger and is not posted again".into(),
        }),
        Some(LinkStatus::Posted) => {
            let bill_external = bill_view
                .as_ref()
                .and_then(|link| link.external_id.clone())
                .filter(|id| !id.is_empty())
                .ok_or_else(|| CommandError::Ledger {
                    code: "conflict",
                    detail: "the posted bill has no ledger id".into(),
                })?;
            let payment_external = match &posted_payment {
                Some((_, link)) => Some(
                    link.external_id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| CommandError::Ledger {
                            code: "conflict",
                            detail: "the posted payment has no ledger id".into(),
                        })?,
                ),
                None => None,
            };
            if cx.preview {
                return Ok(json!({
                    "status": "ready",
                    "external_key": payvy_ledger::bill_key(bill.id),
                    "voids_payment": payment_external.is_some(),
                }));
            }
            let voided = {
                let ledger = cx.ledger()?;
                payvy_ledger::void_posted(ledger, &bill_external, payment_external.as_deref()).await
            };
            match voided {
                Ok(payment_voided) => {
                    if payment_voided && let Some((payment_id, _)) = &posted_payment {
                        store::mark_ledger_link_voided(
                            &mut cx.tx,
                            connection.id,
                            "payment",
                            *payment_id,
                        )
                        .await?;
                    }
                    store::mark_ledger_link_voided(&mut cx.tx, connection.id, "bill", bill.id)
                        .await?;
                    cx.emit(
                        "ledger.voided",
                        bill.id,
                        json!({
                            "bill_id": bill.id,
                            "payment_voided": payment_voided,
                        }),
                    )
                    .await?;
                    Ok(json!({ "status": "voided", "payment_voided": payment_voided }))
                }
                Err(failure) => {
                    if failure.payment_voided {
                        if let Some((payment_id, _)) = &posted_payment {
                            store::mark_ledger_link_voided(
                                &mut cx.tx,
                                connection.id,
                                "payment",
                                *payment_id,
                            )
                            .await?;
                        }
                        store::note_ledger_link_error(
                            &mut cx.tx,
                            connection.id,
                            "bill",
                            bill.id,
                            failure.error.code.as_str(),
                            &failure.error.detail,
                        )
                        .await?;
                    } else if let Some((payment_id, _)) = &posted_payment {
                        store::note_ledger_link_error(
                            &mut cx.tx,
                            connection.id,
                            "payment",
                            *payment_id,
                            failure.error.code.as_str(),
                            &failure.error.detail,
                        )
                        .await?;
                    }
                    cx.emit(
                        "ledger.void_failed",
                        bill.id,
                        json!({
                            "bill_id": bill.id,
                            "error_code": failure.error.code.as_str(),
                            "error_detail": failure.error.detail,
                            "payment_voided": failure.payment_voided,
                        }),
                    )
                    .await?;
                    Err(ledger_error(failure.error))
                }
            }
        }
        _ => {
            if cx.preview {
                return Err(CommandError::Ledger {
                    code: "bill_not_posted",
                    detail: "the bill is not in the ledger yet".into(),
                });
            }
            record_failure(
                cx,
                connection.id,
                "bill",
                bill.id,
                &payvy_ledger::bill_key(bill.id),
                PrepareError::Ledger(LedgerError::new(
                    payvy_ledger::Code::BillNotPosted,
                    "the bill is not in the ledger yet",
                )),
            )
            .await
        }
    }
}

fn plan_value(plan: Result<Plan, PrepareError>) -> Result<Value, CommandError> {
    match plan {
        Ok(Plan::Replay(outcome)) => Ok(posted_json(&outcome)),
        Ok(Plan::Ready { external_key, .. }) => {
            Ok(json!({ "status": "ready", "external_key": external_key }))
        }
        Err(error) => Err(prepare_error(error)),
    }
}

fn posted_json(outcome: &PostedOutcome) -> Value {
    json!({
        "status": "posted",
        "external_id": outcome.posted.external_id,
        "external_key": outcome.posted.external_key,
        "replay": outcome.replay,
    })
}

/// Saves the refusal on the link when the post is not already settled, then
/// returns it. The dispatcher commits that row.
async fn record_failure(
    cx: &mut Cx<'_>,
    connection_id: Uuid,
    entity: &str,
    payvy_id: Uuid,
    external_key: &str,
    error: PrepareError,
) -> Result<Value, CommandError> {
    let error = match error {
        PrepareError::Invalid(message) => return Err(CommandError::Invalid(message)),
        PrepareError::Ledger(error) => error,
    };
    if error.write_link {
        store::put_ledger_link(
            &mut cx.tx,
            cx.org,
            &store::PutLedgerLink {
                connection_id,
                entity,
                payvy_id,
                external_id: None,
                external_key,
                status: "error",
                error_code: Some(error.code.as_str()),
                error_detail: Some(&error.detail),
                request_hash: error.request_hash.as_deref(),
                posted_at: None,
            },
        )
        .await?;
    }
    let kind = if entity == "payment" {
        "ledger.payment_failed"
    } else {
        "ledger.bill_failed"
    };
    let mut payload = json!({
        "error_code": error.code.as_str(),
        "error_detail": error.detail,
    });
    payload[if entity == "payment" {
        "payment_id"
    } else {
        "bill_id"
    }] = json!(payvy_id);
    cx.emit(kind, payvy_id, payload).await?;
    Err(ledger_error(error))
}

fn prepare_error(error: PrepareError) -> CommandError {
    match error {
        PrepareError::Invalid(message) => CommandError::Invalid(message),
        PrepareError::Ledger(error) => ledger_error(error),
    }
}

fn ledger_error(error: LedgerError) -> CommandError {
    CommandError::Ledger {
        code: error.code.as_str(),
        detail: error.detail,
    }
}

fn require_ledger(cx: &Cx<'_>) -> Result<(), CommandError> {
    cx.ledger().map(|_| ())
}

async fn connection(cx: &mut Cx<'_>) -> Result<store::LedgerConnection, CommandError> {
    store::active_ledger_connection(&mut cx.tx)
        .await?
        .ok_or_else(|| CommandError::Ledger {
            code: "needs_auth",
            detail: "Connect a ledger before posting.".into(),
        })
}

fn vendor_external(link: Option<store::LedgerLink>) -> String {
    link.filter(|link| link.status == "linked" || link.status == "posted")
        .and_then(|link| link.external_id)
        .unwrap_or_default()
}

fn view_of(link: &store::LedgerLink) -> Result<LinkView, CommandError> {
    let status = LinkStatus::parse(&link.status).ok_or_else(|| {
        CommandError::Internal(anyhow::anyhow!(
            "unknown ledger link status {}",
            link.status
        ))
    })?;
    Ok(LinkView {
        status,
        external_id: link.external_id.clone(),
        external_key: link.external_key.clone(),
        request_hash: link.request_hash.clone(),
    })
}

fn books_for(
    connection: &store::LedgerConnection,
    records: &[store::LedgerRecord],
    vendor_external_id: &str,
) -> Books {
    let mut accounts = Vec::new();
    let mut periods = Vec::new();
    let mut vendor_currency = None;
    for record in records {
        match record.kind.as_str() {
            "account" => accounts.push(payvy_ledger::account_rec(
                record.external_id.clone(),
                record.inactive,
                &record.detail,
            )),
            "period" => {
                if let Some(period) = payvy_ledger::period_rec(record.inactive, &record.detail) {
                    periods.push(period);
                }
            }
            "vendor" if record.external_id == vendor_external_id => {
                vendor_currency = payvy_ledger::vendor_currency(&record.detail);
            }
            _ => {}
        }
    }
    Books {
        currency: connection.currency.clone(),
        subsidiary_id: connection.subsidiary_id.clone().unwrap_or_default(),
        defaults: payvy_ledger::Defaults::from_json(&connection.defaults),
        accounts,
        periods,
        vendor_currency,
    }
}

fn mapped_account(map: &Value, bank_id: Uuid) -> String {
    map.get(bank_id.to_string())
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|account| !account.is_empty())
        .unwrap_or("")
        .to_owned()
}
