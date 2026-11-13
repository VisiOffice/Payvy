//! Posting to the company's ledger, and connecting one. The token is
//! sealed with PAYVY_SECRETS_KEY and never appears in a result, an event
//! or a log. A VisiBooks connection is built from its row (origin, entity,
//! unsealed key) at post time, per company; other providers use whatever
//! [`payvy_ledger::Ledger`] the server was given (tests pass a fake).

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
    let connection = connection(cx).await?;
    let adapter = adapter_for(cx, &connection).await?;
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
    // The service period the bill covers, from the first allocated PO
    // that names one.
    let mut service_period = None;
    for a in payvy_store::purchase_orders::allocations_for_bill(&mut cx.tx, bill.id).await? {
        if let Some(po) = payvy_store::purchase_orders::get_po(&mut cx.tx, a.po_id).await?
            && let (Some(start), Some(end)) = (po.period_start, po.period_end)
        {
            service_period = Some((start, end));
            break;
        }
    }
    let draft = payvy_ledger::BillDraft {
        bill_id: bill.id,
        service_period,
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
        let ledger = adapter.get();
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
    // Point the ledger's bill at its document in Payvy. Idempotent and
    // not part of the post's hash; a failure is said, never hidden.
    let document_link = match cx.auth() {
        Some(auth) => {
            let url = format!(
                "{}/bills/{}",
                auth.public_url.trim_end_matches('/'),
                bill.id
            );
            match adapter
                .get()
                .set_document_link(&outcome.posted.external_key, &url)
                .await
            {
                Ok(()) => "set",
                Err(e) => {
                    tracing::warn!(bill = %bill.id, "ledger document link failed: {e}");
                    "failed"
                }
            }
        }
        None => "skipped",
    };
    cx.emit(
        "ledger.bill_posted",
        bill.id,
        json!({
            "bill_id": bill.id,
            "external_id": outcome.posted.external_id,
            "external_key": outcome.posted.external_key,
            "replay": outcome.replay,
            "document_link": document_link,
        }),
    )
    .await?;
    let mut out = posted_json(&outcome);
    out["document_link"] = json!(document_link);
    Ok(out)
}

pub async fn post_payment(cx: &mut Cx<'_>, input: PaymentId) -> Result<Value, CommandError> {
    let connection = connection(cx).await?;
    let adapter = adapter_for(cx, &connection).await?;
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
        // The poller can fill transaction_id after this post. It stays out of
        // the memo so a later post is still the same payment.
        memo: payment.method.clone(),
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
        let ledger = adapter.get();
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
    let connection = connection(cx).await?;
    let adapter = adapter_for(cx, &connection).await?;
    let bill = store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    let bill_link = store::ledger_link(&mut cx.tx, connection.id, "bill", bill.id).await?;
    let bill_view = bill_link.as_ref().map(view_of).transpose()?;
    let payments = store::bill_payments(&mut cx.tx, bill.id).await?;
    // A bank return does not call the ledger. The poller only changes the
    // payment row. Every link that is still posted is voided here, including
    // a payment the bank later returned. bill_payments is newest first.
    let mut posted_payments: Vec<(Uuid, String)> = Vec::new();
    for payment in payments.iter().rev() {
        let Some(link) =
            store::ledger_link(&mut cx.tx, connection.id, "payment", payment.id).await?
        else {
            continue;
        };
        if link.status != "posted" {
            continue;
        }
        let external = link
            .external_id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| CommandError::Ledger {
                code: "conflict",
                detail: "the posted payment has no ledger id".into(),
            })?;
        posted_payments.push((payment.id, external));
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
            if cx.preview {
                return Ok(json!({
                    "status": "ready",
                    "external_key": payvy_ledger::bill_key(bill.id),
                    "payments_to_void": posted_payments.len(),
                }));
            }
            let payment_ids: Vec<&str> =
                posted_payments.iter().map(|(_, id)| id.as_str()).collect();
            let voided = {
                let ledger = adapter.get();
                payvy_ledger::void_posted(ledger, &bill_external, &payment_ids).await
            };
            match voided {
                Ok(payments_voided) => {
                    for (payment_id, _) in &posted_payments {
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
                            "payments_voided": payments_voided,
                        }),
                    )
                    .await?;
                    Ok(json!({ "status": "voided", "payments_voided": payments_voided }))
                }
                Err(failure) => {
                    let n = failure.payments_voided;
                    for (payment_id, _) in posted_payments.iter().take(n) {
                        store::mark_ledger_link_voided(
                            &mut cx.tx,
                            connection.id,
                            "payment",
                            *payment_id,
                        )
                        .await?;
                    }
                    if n < posted_payments.len() {
                        let (payment_id, _) = posted_payments[n];
                        store::note_ledger_link_error(
                            &mut cx.tx,
                            connection.id,
                            "payment",
                            payment_id,
                            failure.error.code.as_str(),
                            &failure.error.detail,
                        )
                        .await?;
                    } else {
                        store::note_ledger_link_error(
                            &mut cx.tx,
                            connection.id,
                            "bill",
                            bill.id,
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
                            "payments_voided": n,
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

/// The adapter for this connection: VisiBooks is built from the row
/// (origin, entity, unsealed key); anything else uses the server's
/// configured adapter, which production leaves unset for now.
enum Adapter<'a> {
    Static(&'a (dyn payvy_ledger::Ledger + Send + Sync)),
    Built(Box<dyn payvy_ledger::Ledger + Send + Sync>),
}

impl Adapter<'_> {
    fn get(&self) -> &(dyn payvy_ledger::Ledger + Send + Sync) {
        match self {
            Adapter::Static(l) => *l,
            Adapter::Built(l) => l.as_ref(),
        }
    }
}

async fn adapter_for<'s>(
    cx: &Cx<'s>,
    connection: &store::LedgerConnection,
) -> Result<Adapter<'s>, CommandError> {
    if connection.provider != "visibooks" {
        return Ok(Adapter::Static(cx.ledger()?));
    }
    let origin = connection
        .origin
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            CommandError::NotConfigured(
                "the VisiBooks connection has no origin; an admin sets it in Settings → Ledger",
            )
        })?;
    let bank = cx.bank()?;
    let token = bank.sealer.open(
        cx.org,
        &payvy_ledger::seal_purpose("visibooks"),
        &connection.token_sealed,
    )?;
    let adapter = payvy_ledger::VisiBooks::new(origin, &connection.account_id, &token)
        .map_err(|e| CommandError::Invalid(e.to_string()))?;
    Ok(Adapter::Built(Box::new(adapter)))
}

/// Locks the connection for the whole command, including the ledger call.
async fn connection(cx: &mut Cx<'_>) -> Result<store::LedgerConnection, CommandError> {
    match store::active_ledger_connection(&mut cx.tx).await? {
        Some(c) => Ok(c),
        // A configured adapter without a connection (tests) still says
        // "connect one"; no adapter at all says it isn't configured.
        None if cx.ledger().is_ok() => Err(CommandError::Ledger {
            code: "needs_auth",
            detail: "Connect a ledger before posting.".into(),
        }),
        None => Err(CommandError::NotConfigured("no ledger is configured")),
    }
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

// ── Connection settings (Settings → Ledger) ──────────────────────────────

fn connection_view(connection: Option<&store::LedgerConnection>) -> Value {
    match connection {
        Some(c) => json!({
            "connected": true,
            "provider": c.provider,
            "origin": c.origin,
            "entity": c.account_id,
            "environment": c.environment,
            "post_when": c.post_when,
            "ap_account_id": c.ap_account_id,
            "defaults": c.defaults,
            "bank_account_map": c.bank_account_map,
            "status": c.status,
        }),
        None => json!({ "connected": false }),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {}

pub async fn settings(cx: &mut Cx<'_>, _input: Settings) -> Result<Value, CommandError> {
    let connection = store::active_ledger_connection(&mut cx.tx).await?;
    Ok(connection_view(connection.as_ref()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connect {
    origin: String,
    entity: String,
    api_key: String,
    post_when: Option<String>,
}

fn valid_post_when(value: Option<&str>) -> Result<String, CommandError> {
    match value.unwrap_or("approval") {
        "approval" | "payment" => Ok(value.unwrap_or("approval").to_owned()),
        other => Err(CommandError::Invalid(format!(
            "post_when is approval or payment, not {other}"
        ))),
    }
}

/// Connect VisiBooks for this company (Admin, person only). The API key
/// is sealed at once and never read back; reconnecting updates the same
/// connection row, so its links stand.
pub async fn connect(cx: &mut Cx<'_>, input: Connect) -> Result<Value, CommandError> {
    let origin = input.origin.trim().trim_end_matches('/').to_owned();
    if !(origin.starts_with("https://") || origin.starts_with("http://127.0.0.1")) {
        return Err(CommandError::Invalid(
            "the origin must be an https URL (http is allowed only for 127.0.0.1 in development)"
                .into(),
        ));
    }
    let entity = input.entity.trim().to_owned();
    if entity.is_empty() || input.api_key.trim().is_empty() {
        return Err(CommandError::Invalid(
            "an entity id and an API key are required".into(),
        ));
    }
    let post_when = valid_post_when(input.post_when.as_deref())?;
    let bank = cx.bank()?;
    let sealed = bank.sealer.seal(
        cx.org,
        &payvy_ledger::seal_purpose("visibooks"),
        input.api_key.trim(),
    )?;
    let existing = store::active_ledger_connection(&mut cx.tx).await?;
    let id = match &existing {
        Some(c) if c.provider == "visibooks" => {
            store::update_ledger_connection(
                &mut cx.tx,
                c.id,
                Some(&origin),
                &entity,
                &c.post_when,
                c.ap_account_id.as_deref(),
                &c.defaults,
                &c.bank_account_map,
                Some(&sealed),
            )
            .await?;
            c.id
        }
        Some(c) => {
            return Err(CommandError::Conflict(format!(
                "a {} ledger is already connected for this company; this build connects VisiBooks",
                c.provider
            )));
        }
        None => {
            store::insert_ledger_connection(
                &mut cx.tx,
                cx.org,
                &store::NewLedgerConnection {
                    id: Uuid::new_v4(),
                    origin: Some(&origin),
                    provider: "visibooks",
                    environment: "production",
                    account_id: &entity,
                    subsidiary_id: None,
                    subsidiary_name: None,
                    currency: "USD",
                    status: "active",
                    auth_kind: "client_credentials",
                    token_sealed: &sealed,
                    post_when: &post_when,
                    ap_account_id: None,
                    defaults: &json!({}),
                    bank_account_map: &json!({}),
                    created_at: cx.now,
                    created_by: &cx.actor.name,
                },
            )
            .await?
            .id
        }
    };
    cx.emit(
        "ledger.connected",
        id,
        json!({ "provider": "visibooks", "origin": origin, "entity": entity, "post_when": post_when }),
    )
    .await?;
    let connection = store::active_ledger_connection(&mut cx.tx).await?;
    Ok(connection_view(connection.as_ref()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configure {
    post_when: Option<String>,
    ap_account_id: Option<Option<String>>,
    defaults: Option<Value>,
    bank_account_map: Option<Value>,
}

/// Change how the connected ledger posts (Admin, person only). Asking
/// for what is already set is refused, never a bare success.
pub async fn configure(cx: &mut Cx<'_>, input: Configure) -> Result<Value, CommandError> {
    let connection = store::active_ledger_connection(&mut cx.tx)
        .await?
        .ok_or(CommandError::NotFound("ledger connection"))?;
    let post_when = match &input.post_when {
        Some(v) => valid_post_when(Some(v))?,
        None => connection.post_when.clone(),
    };
    let ap = match &input.ap_account_id {
        Some(v) => v.clone(),
        None => connection.ap_account_id.clone(),
    };
    let defaults = input
        .defaults
        .unwrap_or_else(|| connection.defaults.clone());
    let map = input
        .bank_account_map
        .unwrap_or_else(|| connection.bank_account_map.clone());
    if post_when == connection.post_when
        && ap == connection.ap_account_id
        && defaults == connection.defaults
        && map == connection.bank_account_map
    {
        return Err(CommandError::Conflict(
            "these ledger settings are already in force".into(),
        ));
    }
    store::update_ledger_connection(
        &mut cx.tx,
        connection.id,
        connection.origin.as_deref(),
        &connection.account_id,
        &post_when,
        ap.as_deref(),
        &defaults,
        &map,
        None,
    )
    .await?;
    cx.emit(
        "ledger.configured",
        connection.id,
        json!({ "post_when": post_when, "bank_account_map": map }),
    )
    .await?;
    let connection = store::active_ledger_connection(&mut cx.tx).await?;
    Ok(connection_view(connection.as_ref()))
}

// ── Automatic posting (post_when hooks) ──────────────────────────────────

async fn active_connection(cx: &mut Cx<'_>) -> Option<store::LedgerConnection> {
    if cx.preview {
        return None;
    }
    store::active_ledger_connection(&mut cx.tx)
        .await
        .ok()
        .flatten()
}

/// When a bill's approval completes and the company posts at approval,
/// post it. A failure is recorded on the bill's ledger link by the post
/// itself; the approval stands either way.
pub async fn on_bill_approved(cx: &mut Cx<'_>, bill_id: Uuid) {
    let Some(connection) = active_connection(cx).await else {
        return;
    };
    if connection.post_when != "approval" {
        return;
    }
    if let Err(e) = post_bill(cx, BillId { bill_id }).await {
        tracing::warn!(bill = %bill_id, "ledger post at approval failed: {e}");
    }
}

/// When a bank payment is sent: in payment mode the bill posts first,
/// then the payment; in approval mode only the payment remains. Then the
/// bank's reference follows, whenever it was learned. Failures are
/// recorded by the posts themselves and never touch the money facts.
pub async fn on_payment_sent(cx: &mut Cx<'_>, payment: &store::PaymentRow, bill: &store::Bill) {
    let Some(connection) = active_connection(cx).await else {
        return;
    };
    if connection.post_when == "payment"
        && let Err(e) = post_bill(cx, BillId { bill_id: bill.id }).await
    {
        tracing::warn!(bill = %bill.id, "ledger bill post at payment failed: {e}");
    }
    match post_payment(
        cx,
        PaymentId {
            payment_id: payment.id,
        },
    )
    .await
    {
        Ok(_) => on_bank_reference(cx, payment).await,
        Err(e) => {
            tracing::warn!(payment = %payment.id, "ledger payment post failed: {e}");
        }
    }
}

/// Tell the ledger the bank's reference for an already-posted payment.
/// Quiet when there is nothing to tell or nowhere to tell it.
pub async fn on_bank_reference(cx: &mut Cx<'_>, payment: &store::PaymentRow) {
    let Some(reference) = payment.transaction_id.as_deref() else {
        return;
    };
    let Some(connection) = active_connection(cx).await else {
        return;
    };
    let posted = store::ledger_link(&mut cx.tx, connection.id, "payment", payment.id)
        .await
        .ok()
        .flatten()
        .is_some_and(|l| l.external_id.is_some() && l.status == "posted");
    if !posted {
        return;
    }
    match adapter_for(cx, &connection).await {
        Ok(adapter) => {
            let key = payvy_ledger::payment_key(payment.id);
            if let Err(e) = adapter.get().set_bank_reference(&key, reference).await {
                tracing::warn!(payment = %payment.id, "ledger bank reference failed: {e}");
            }
        }
        Err(e) => tracing::warn!(payment = %payment.id, "ledger adapter unavailable: {e}"),
    }
}

/// A bill paid outside Payvy (`bill.mark_paid`): in payment mode its bill
/// posts now. The payment leg itself has no Payvy payment row and no
/// bank connection to map an account from, so it is not posted; the skip
/// is an event on the bill, never silent. Bank-sent payments post in
/// full through `on_payment_sent`.
pub async fn on_outside_payment(cx: &mut Cx<'_>, bill: &store::Bill) {
    let Some(connection) = active_connection(cx).await else {
        return;
    };
    if connection.post_when == "payment"
        && let Err(e) = post_bill(cx, BillId { bill_id: bill.id }).await
    {
        tracing::warn!(bill = %bill.id, "ledger bill post at outside payment failed: {e}");
        return;
    }
    if connection.post_when == "payment" {
        let _ = cx
            .emit(
                "ledger.payment_skipped",
                bill.id,
                json!({ "bill_id": bill.id,
                        "reason": "paid outside Payvy: no bank account mapping for an outside payment" }),
            )
            .await;
    }
}
