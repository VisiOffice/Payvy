//! The ledger a company keeps. Payvy holds the bill and the payment it
//! already sent through the customer's bank. A [`Ledger`] records them.
//! NetSuite, VisiBooks, QuickBooks and Xero implement the trait. None of
//! them are asked to move money.
//!
//! A repeat of the same request hash returns the first id and does not call
//! the ledger again. A different hash after a post is a conflict. When a key
//! the adapter already accepted arrives with a different body, the adapter
//! refuses it and does not return the first id. This crate does not open a
//! network connection: the server supplies a [`Ledger`], and tests supply
//! [`FakeLedger`].

mod fake;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub use fake::FakeLedger;

/// How many times a busy ledger is tried before the refusal is returned.
pub const CONCURRENCY_ATTEMPTS: u32 = 3;

/// How long one ledger call may take. The connection row stays locked across
/// the call, so a hung adapter must not hold every other ledger action.
pub const LEDGER_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Stable codes a person can act on. `bill_not_posted` is Payvy's own: the
/// payment was asked for before the bill had a posted link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    NeedsAuth,
    PeriodClosed,
    MissingField,
    UnknownAccount,
    VendorUnlinked,
    Currency,
    DuplicateInvoice,
    Conflict,
    Concurrency,
    Timeout,
    PaymentUnmapped,
    BillNotPosted,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsAuth => "needs_auth",
            Self::PeriodClosed => "period_closed",
            Self::MissingField => "missing_field",
            Self::UnknownAccount => "unknown_account",
            Self::VendorUnlinked => "vendor_unlinked",
            Self::Currency => "currency",
            Self::DuplicateInvoice => "duplicate_invoice",
            Self::Conflict => "conflict",
            Self::Concurrency => "concurrency",
            Self::Timeout => "timeout",
            Self::PaymentUnmapped => "payment_unmapped",
            Self::BillNotPosted => "bill_not_posted",
        }
    }
}

/// Why a post or void stopped. `write_link` is false when a posted or voided
/// link must be left as it is (a replay is not an error; a conflict is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerError {
    pub code: Code,
    pub detail: String,
    pub request_hash: Option<String>,
    pub write_link: bool,
}

impl LedgerError {
    pub fn new(code: Code, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            request_hash: None,
            write_link: true,
        }
    }

    pub fn keep_link(mut self) -> Self {
        self.write_link = false;
        self
    }

    pub fn with_hash(mut self, hash: String) -> Self {
        self.request_hash = Some(hash);
        self
    }
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for LedgerError {}

/// AAD for a sealed ledger credential: `ledger:<provider>`. The plaintext
/// never goes in a command, an event, or a log.
pub fn seal_purpose(provider: &str) -> String {
    format!("ledger:{provider}")
}

pub fn vendor_key(id: Uuid) -> String {
    format!("payvy:vendor:{id}")
}

pub fn bill_key(id: Uuid) -> String {
    format!("payvy:bill:{id}")
}

pub fn payment_key(id: Uuid) -> String {
    format!("payvy:payment:{id}")
}

/// Two decimal places, from integer cents. Never a binary float.
pub fn cents_to_amount(cents: i64) -> String {
    let value = i128::from(cents);
    let negative = value < 0;
    let value = value.unsigned_abs();
    let (dollars, rem) = (value / 100, value % 100);
    if negative {
        format!("-{dollars}.{rem:02}")
    } else {
        format!("{dollars}.{rem:02}")
    }
}

/// sha256 of the canonical JSON. Struct field order is the order serde writes.
pub fn body_hash(body: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(body).expect("ledger body is plain data");
    hex::encode(Sha256::digest(bytes))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    pub external_id: String,
    pub external_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedOutcome {
    pub posted: Posted,
    pub replay: bool,
    pub hash: String,
}

/// What a preview can know without calling the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Replay(PostedOutcome),
    Ready { external_key: String, hash: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStatus {
    Linked,
    Posted,
    Error,
    Voided,
}

impl LinkStatus {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "linked" => Self::Linked,
            "posted" => Self::Posted,
            "error" => Self::Error,
            "voided" => Self::Voided,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::Posted => "posted",
            Self::Error => "error",
            Self::Voided => "voided",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LinkView {
    pub status: LinkStatus,
    pub external_id: Option<String>,
    pub external_key: String,
    pub request_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DraftLine {
    pub account_id: String,
    pub amount_cents: i64,
    pub memo: String,
    pub department_id: Option<String>,
    pub class_id: Option<String>,
    pub location_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BillDraft {
    pub bill_id: Uuid,
    pub vendor_external_id: String,
    pub invoice_number: String,
    pub invoice_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub memo: String,
    pub amount_cents: i64,
    pub lines: Vec<DraftLine>,
}

#[derive(Debug, Clone)]
pub struct PaymentDraft {
    pub payment_id: Uuid,
    pub amount_cents: i64,
    pub tran_date: NaiveDate,
    /// The payment method only. A bank reference that arrives later is not
    /// part of the hash or the body sent to the ledger.
    pub memo: String,
    /// Empty when the Payvy bank connection has no ledger account yet.
    pub bank_account_id: String,
    /// Empty when the bill has no posted link.
    pub bill_external_id: String,
    pub vendor_external_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BillLineBody {
    pub account_id: String,
    pub amount: String,
    pub memo: String,
    pub department_id: Option<String>,
    pub class_id: Option<String>,
    pub location_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BillBody {
    pub external_key: String,
    pub vendor_external_id: String,
    pub subsidiary_id: String,
    pub invoice_number: String,
    pub invoice_date: String,
    pub due_date: Option<String>,
    pub memo: String,
    pub currency: String,
    pub lines: Vec<BillLineBody>,
}

/// What a first post sends. `account_id` is the bank mapping at that moment.
/// The stored request hash omits it, so removing the mapping later still
/// replays the first id. Remapping does not post a second payment: the books
/// already have the first account.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentBody {
    pub external_key: String,
    pub bill_external_id: String,
    pub vendor_external_id: String,
    pub subsidiary_id: String,
    pub account_id: String,
    pub tran_date: String,
    pub memo: String,
    pub currency: String,
    pub amount: String,
}

#[derive(Debug, Clone, Default)]
pub struct Defaults {
    pub expense_account_id: Option<String>,
    pub department_id: Option<String>,
    pub class_id: Option<String>,
    pub location_id: Option<String>,
    /// Any other string the company set, keyed by the field name.
    pub extra: BTreeMap<String, String>,
}

impl Defaults {
    pub fn from_json(value: &Value) -> Self {
        let mut extra = BTreeMap::new();
        if let Some(object) = value.as_object() {
            for (key, item) in object {
                let Some(text) = text_of(item) else { continue };
                match key.as_str() {
                    "expense_account_id" | "department_id" | "class_id" | "location_id" => {}
                    other => {
                        extra.insert(other.to_owned(), text);
                    }
                }
            }
        }
        Self {
            expense_account_id: string_field(value, "expense_account_id"),
            department_id: string_field(value, "department_id"),
            class_id: string_field(value, "class_id"),
            location_id: string_field(value, "location_id"),
            extra,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccountRec {
    pub external_id: String,
    pub inactive: bool,
    /// Field names the account cannot post without.
    pub requires: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PeriodRec {
    pub start: NaiveDate,
    pub end: NaiveDate,
    /// Closed, or the period record is inactive.
    pub closed: bool,
}

#[derive(Debug, Clone)]
pub struct Books {
    pub currency: String,
    pub subsidiary_id: String,
    pub defaults: Defaults,
    pub accounts: Vec<AccountRec>,
    pub periods: Vec<PeriodRec>,
    pub vendor_currency: Option<String>,
}

pub fn account_rec(external_id: String, inactive: bool, detail: &Value) -> AccountRec {
    let requires = detail
        .get("requires")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(text_of).collect())
        .unwrap_or_default();
    AccountRec {
        external_id,
        inactive,
        requires,
    }
}

/// None when the cache row has no start and end. A missing catalog is an
/// open period, so phase one can post before a period list exists.
pub fn period_rec(inactive: bool, detail: &Value) -> Option<PeriodRec> {
    let start = date_field(detail, "start")?;
    let end = date_field(detail, "end")?;
    let closed = inactive
        || detail
            .get("closed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    Some(PeriodRec { start, end, closed })
}

pub fn vendor_currency(detail: &Value) -> Option<String> {
    string_field(detail, "currency")
}

#[derive(Debug)]
pub enum PrepareError {
    Invalid(String),
    Ledger(LedgerError),
}

/// The id the ledger assigned, or the id already stored for this key.
pub trait Ledger: Send + Sync {
    fn post_bill<'a>(
        &'a self,
        body: &'a BillBody,
    ) -> Pin<Box<dyn Future<Output = Result<Posted, LedgerError>> + Send + 'a>>;

    fn post_payment<'a>(
        &'a self,
        body: &'a PaymentBody,
    ) -> Pin<Box<dyn Future<Output = Result<Posted, LedgerError>> + Send + 'a>>;

    fn void_bill<'a>(
        &'a self,
        external_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LedgerError>> + Send + 'a>>;

    fn void_payment<'a>(
        &'a self,
        external_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LedgerError>> + Send + 'a>>;

    /// The ledger's file id. The same bill and name with the same bytes returns
    /// the id already stored. Different bytes for that name are a conflict.
    fn attach_file<'a>(
        &'a self,
        bill_external_id: &'a str,
        name: &'a str,
        bytes: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<String, LedgerError>> + Send + 'a>>;
}

pub fn plan_bill(
    books: &Books,
    draft: &BillDraft,
    existing: Option<&LinkView>,
) -> Result<Plan, PrepareError> {
    Ok(match decide_bill(books, draft, existing)? {
        Decision::Replay(outcome) => Plan::Replay(outcome),
        Decision::Send { body, hash } => Plan::Ready {
            external_key: body.external_key,
            hash,
        },
    })
}

pub async fn submit_bill(
    ledger: &dyn Ledger,
    books: &Books,
    draft: &BillDraft,
    existing: Option<&LinkView>,
) -> Result<PostedOutcome, PrepareError> {
    match decide_bill(books, draft, existing)? {
        Decision::Replay(outcome) => Ok(outcome),
        Decision::Send { body, hash } => match send_post(ledger, Post::Bill(&body)).await {
            Ok(posted) => Ok(PostedOutcome {
                posted,
                replay: false,
                hash,
            }),
            Err(error) => Err(PrepareError::Ledger(error.with_hash(hash))),
        },
    }
}

pub fn plan_payment(
    books: &Books,
    draft: &PaymentDraft,
    existing: Option<&LinkView>,
) -> Result<Plan, PrepareError> {
    Ok(match decide_payment(books, draft, existing)? {
        DecisionPay::Replay(outcome) => Plan::Replay(outcome),
        DecisionPay::Send { body, hash } => Plan::Ready {
            external_key: body.external_key,
            hash,
        },
    })
}

pub async fn submit_payment(
    ledger: &dyn Ledger,
    books: &Books,
    draft: &PaymentDraft,
    existing: Option<&LinkView>,
) -> Result<PostedOutcome, PrepareError> {
    match decide_payment(books, draft, existing)? {
        DecisionPay::Replay(outcome) => Ok(outcome),
        DecisionPay::Send { body, hash } => match send_post(ledger, Post::Payment(&body)).await {
            Ok(posted) => Ok(PostedOutcome {
                posted,
                replay: false,
                hash,
            }),
            Err(error) => Err(PrepareError::Ledger(error.with_hash(hash))),
        },
    }
}

#[derive(Debug)]
pub struct VoidFailure {
    /// How many leading payment ids this call voided before it stopped.
    pub payments_voided: usize,
    pub error: LedgerError,
}

/// Voids each posted payment, oldest first, then the bill.
///
/// A bank return does not call the ledger on its own. The caller passes every
/// payment link that is still posted, including a payment the bank later
/// returned, so the books do not keep it against a voided bill. Reversing a
/// return by itself is this same void of that one payment, and it waits for
/// a real adapter.
///
/// If one payment void is refused, later payments and the bill stay posted.
/// Payments already voided in this call stay voided.
pub async fn void_posted(
    ledger: &dyn Ledger,
    bill_external_id: &str,
    payment_external_ids: &[&str],
) -> Result<usize, VoidFailure> {
    let mut voided = 0;
    for payment in payment_external_ids {
        if let Err(error) = send_void(ledger, Void::Payment(payment)).await {
            return Err(VoidFailure {
                payments_voided: voided,
                error,
            });
        }
        voided += 1;
    }
    if let Err(error) = send_void(ledger, Void::Bill(bill_external_id)).await {
        return Err(VoidFailure {
            payments_voided: voided,
            error,
        });
    }
    Ok(voided)
}

enum Decision {
    Replay(PostedOutcome),
    Send { body: BillBody, hash: String },
}

enum DecisionPay {
    Replay(PostedOutcome),
    Send { body: PaymentBody, hash: String },
}

fn decide_bill(
    books: &Books,
    draft: &BillDraft,
    existing: Option<&LinkView>,
) -> Result<Decision, PrepareError> {
    // The hash is compared before the books are checked. A closed period, an
    // inactive account, or a missing vendor must not turn a replay into an error.
    let (body, hash) = compose_bill(books, draft);
    match gate(existing, &hash, &body.external_key).map_err(PrepareError::Ledger)? {
        Gate::Replay(posted) => Ok(Decision::Replay(PostedOutcome {
            posted,
            replay: true,
            hash,
        })),
        Gate::Ready => {
            validate_bill(books, draft, &body)?;
            Ok(Decision::Send { body, hash })
        }
    }
}

fn decide_payment(
    books: &Books,
    draft: &PaymentDraft,
    existing: Option<&LinkView>,
) -> Result<DecisionPay, PrepareError> {
    let (body, hash) = compose_payment(books, draft);
    match gate(existing, &hash, &body.external_key).map_err(PrepareError::Ledger)? {
        Gate::Replay(posted) => Ok(DecisionPay::Replay(PostedOutcome {
            posted,
            replay: true,
            hash,
        })),
        Gate::Ready => {
            validate_payment(books, draft)?;
            Ok(DecisionPay::Send { body, hash })
        }
    }
}

enum Gate {
    Replay(Posted),
    Ready,
}

fn gate(existing: Option<&LinkView>, hash: &str, external_key: &str) -> Result<Gate, LedgerError> {
    let Some(link) = existing else {
        return Ok(Gate::Ready);
    };
    match link.status {
        LinkStatus::Posted if link.request_hash.as_deref() == Some(hash) => {
            let external_id = link
                .external_id
                .clone()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| {
                    LedgerError::new(Code::Conflict, "the posted link has no ledger id").keep_link()
                })?;
            let external_key = if link.external_key.is_empty() {
                external_key.to_owned()
            } else {
                link.external_key.clone()
            };
            Ok(Gate::Replay(Posted {
                external_id,
                external_key,
            }))
        }
        LinkStatus::Posted => {
            Err(LedgerError::new(Code::Conflict, "This changed after it was posted").keep_link())
        }
        LinkStatus::Voided => Err(LedgerError::new(
            Code::Conflict,
            "This was voided in the ledger and is not posted again",
        )
        .keep_link()),
        LinkStatus::Linked | LinkStatus::Error => Ok(Gate::Ready),
    }
}

/// The stored payment hash. The bank account is left out: it is books
/// configuration and can disappear after the payment is posted.
#[derive(Serialize)]
struct PaymentIdentity {
    external_key: String,
    bill_external_id: String,
    vendor_external_id: String,
    subsidiary_id: String,
    tran_date: String,
    memo: String,
    currency: String,
    amount: String,
}

fn compose_bill(books: &Books, draft: &BillDraft) -> (BillBody, String) {
    let body = BillBody {
        external_key: bill_key(draft.bill_id),
        vendor_external_id: draft.vendor_external_id.trim().to_owned(),
        subsidiary_id: books.subsidiary_id.trim().to_owned(),
        invoice_number: draft.invoice_number.clone(),
        invoice_date: draft.invoice_date.to_string(),
        due_date: draft.due_date.map(|date| date.to_string()),
        memo: draft.memo.clone(),
        currency: "USD".to_owned(),
        lines: compose_lines(books, draft),
    };
    let hash = body_hash(&body);
    (body, hash)
}

fn compose_lines(books: &Books, draft: &BillDraft) -> Vec<BillLineBody> {
    let source = if draft.lines.is_empty() {
        vec![DraftLine {
            account_id: String::new(),
            amount_cents: draft.amount_cents,
            memo: draft.memo.clone(),
            department_id: None,
            class_id: None,
            location_id: None,
        }]
    } else {
        draft.lines.clone()
    };
    source
        .iter()
        .map(|line| BillLineBody {
            account_id: filled(&Some(line.account_id.clone()))
                .or_else(|| books.defaults.expense_account_id.clone())
                .unwrap_or_default(),
            amount: cents_to_amount(line.amount_cents),
            memo: line.memo.clone(),
            department_id: segment(line, &books.defaults, "department"),
            class_id: segment(line, &books.defaults, "class"),
            location_id: segment(line, &books.defaults, "location"),
        })
        .collect()
}

fn validate_bill(books: &Books, draft: &BillDraft, body: &BillBody) -> Result<(), PrepareError> {
    check_books(books)?;
    if body.vendor_external_id.is_empty() {
        return Err(ledger(
            Code::VendorUnlinked,
            "confirm the vendor's ledger match before posting",
        ));
    }
    if draft.amount_cents <= 0 {
        return Err(PrepareError::Invalid(
            "amount_cents must be more than zero".into(),
        ));
    }
    let cents: Vec<i64> = if draft.lines.is_empty() {
        vec![draft.amount_cents]
    } else {
        draft.lines.iter().map(|line| line.amount_cents).collect()
    };
    let sum = cents.into_iter().try_fold(0i64, i64::checked_add);
    let Some(sum) = sum else {
        return Err(PrepareError::Invalid(
            "line amounts exceed the supported range".into(),
        ));
    };
    if sum != draft.amount_cents {
        return Err(PrepareError::Invalid(format!(
            "the lines add up to {sum} cents but the bill is {} cents",
            draft.amount_cents
        )));
    }
    if period_closed(&books.periods, draft.invoice_date) {
        return Err(ledger(
            Code::PeriodClosed,
            format!(
                "the bill date {} is in a closed accounting period",
                draft.invoice_date
            ),
        ));
    }
    for line in &body.lines {
        validate_line(books, line)?;
    }
    Ok(())
}

fn compose_payment(books: &Books, draft: &PaymentDraft) -> (PaymentBody, String) {
    let identity = PaymentIdentity {
        external_key: payment_key(draft.payment_id),
        bill_external_id: draft.bill_external_id.trim().to_owned(),
        vendor_external_id: draft.vendor_external_id.trim().to_owned(),
        subsidiary_id: books.subsidiary_id.trim().to_owned(),
        tran_date: draft.tran_date.to_string(),
        memo: draft.memo.clone(),
        currency: "USD".to_owned(),
        amount: cents_to_amount(draft.amount_cents),
    };
    let hash = body_hash(&identity);
    let body = PaymentBody {
        external_key: identity.external_key.clone(),
        bill_external_id: identity.bill_external_id.clone(),
        vendor_external_id: identity.vendor_external_id.clone(),
        subsidiary_id: identity.subsidiary_id.clone(),
        account_id: draft.bank_account_id.trim().to_owned(),
        tran_date: identity.tran_date,
        memo: identity.memo,
        currency: identity.currency,
        amount: identity.amount,
    };
    (body, hash)
}

fn validate_payment(books: &Books, draft: &PaymentDraft) -> Result<(), PrepareError> {
    check_books(books)?;
    if draft.vendor_external_id.trim().is_empty() {
        return Err(ledger(
            Code::VendorUnlinked,
            "confirm the vendor's ledger match before posting",
        ));
    }
    if draft.bill_external_id.trim().is_empty() {
        return Err(ledger(
            Code::BillNotPosted,
            "the bill is not in the ledger yet",
        ));
    }
    if draft.bank_account_id.trim().is_empty() {
        return Err(ledger(
            Code::PaymentUnmapped,
            "map the bank connection before this payment can post",
        ));
    }
    if draft.amount_cents <= 0 {
        return Err(PrepareError::Invalid(
            "amount_cents must be more than zero".into(),
        ));
    }
    if period_closed(&books.periods, draft.tran_date) {
        return Err(ledger(
            Code::PeriodClosed,
            format!(
                "the payment date {} is in a closed accounting period",
                draft.tran_date
            ),
        ));
    }
    Ok(())
}

fn period_closed(periods: &[PeriodRec], date: NaiveDate) -> bool {
    periods
        .iter()
        .any(|period| date >= period.start && date <= period.end && period.closed)
}

fn check_books(books: &Books) -> Result<(), PrepareError> {
    if books.currency != "USD" {
        return Err(ledger(
            Code::Currency,
            format!(
                "this connection's currency is {}, and Payvy posts USD only",
                books.currency
            ),
        ));
    }
    if let Some(currency) = &books.vendor_currency
        && currency != "USD"
    {
        return Err(ledger(
            Code::Currency,
            format!("this vendor's currency is {currency}, and Payvy posts USD only"),
        ));
    }
    if books.subsidiary_id.trim().is_empty() {
        return Err(ledger(
            Code::MissingField,
            "subsidiary is required and is not set",
        ));
    }
    Ok(())
}

fn validate_line(books: &Books, line: &BillLineBody) -> Result<(), PrepareError> {
    if line.account_id.is_empty() {
        return Err(ledger(
            Code::MissingField,
            "account is required and is not set",
        ));
    }
    let account_id = &line.account_id;
    let Some(account) = books
        .accounts
        .iter()
        .find(|account| account.external_id == *account_id)
    else {
        return Err(ledger(
            Code::UnknownAccount,
            format!("account {account_id} is not in the ledger cache"),
        ));
    };
    if account.inactive {
        return Err(ledger(
            Code::UnknownAccount,
            format!("account {account_id} is inactive"),
        ));
    }
    for name in &account.requires {
        let present = match name.as_str() {
            "department" => line.department_id.is_some(),
            "class" => line.class_id.is_some(),
            "location" => line.location_id.is_some(),
            other => books.defaults.extra.contains_key(other),
        };
        if !present {
            return Err(ledger(
                Code::MissingField,
                format!("{name} is required and is not set"),
            ));
        }
    }
    Ok(())
}

fn segment(line: &DraftLine, defaults: &Defaults, name: &str) -> Option<String> {
    let on_line = match name {
        "department" => filled(&line.department_id),
        "class" => filled(&line.class_id),
        "location" => filled(&line.location_id),
        _ => None,
    };
    if on_line.is_some() {
        return on_line;
    }
    match name {
        "department" => filled(&defaults.department_id),
        "class" => filled(&defaults.class_id),
        "location" => filled(&defaults.location_id),
        _ => None,
    }
}

fn ledger(code: Code, detail: impl Into<String>) -> PrepareError {
    PrepareError::Ledger(LedgerError::new(code, detail))
}

fn filled(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn text_of(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(text_of)
}

fn date_field(value: &Value, key: &str) -> Option<NaiveDate> {
    let text = value.get(key)?.as_str()?;
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

#[derive(Clone, Copy)]
enum Post<'a> {
    Bill(&'a BillBody),
    Payment(&'a PaymentBody),
}

#[derive(Clone, Copy)]
enum Void<'a> {
    Bill(&'a str),
    Payment(&'a str),
}

async fn send_post(ledger: &dyn Ledger, op: Post<'_>) -> Result<Posted, LedgerError> {
    let mut last = None;
    for attempt in 1..=CONCURRENCY_ATTEMPTS {
        let pending = match op {
            Post::Bill(body) => ledger.post_bill(body),
            Post::Payment(body) => ledger.post_payment(body),
        };
        match wait_for(pending).await {
            Ok(posted) => return Ok(posted),
            Err(error) if busy(&error, attempt) => {
                last = Some(error);
                pause(attempt).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last.expect("a busy ledger was tried"))
}

async fn send_void(ledger: &dyn Ledger, op: Void<'_>) -> Result<(), LedgerError> {
    let mut last = None;
    for attempt in 1..=CONCURRENCY_ATTEMPTS {
        let pending = match op {
            Void::Bill(id) => ledger.void_bill(id),
            Void::Payment(id) => ledger.void_payment(id),
        };
        match wait_for(pending).await {
            Ok(()) => return Ok(()),
            Err(error) if busy(&error, attempt) => {
                last = Some(error);
                pause(attempt).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last.expect("a busy ledger was tried"))
}

async fn wait_for<T>(
    pending: Pin<Box<dyn Future<Output = Result<T, LedgerError>> + Send + '_>>,
) -> Result<T, LedgerError> {
    match tokio::time::timeout(LEDGER_CALL_TIMEOUT, pending).await {
        Ok(result) => result,
        Err(_elapsed) => Err(LedgerError::new(
            Code::Timeout,
            "the ledger did not answer in time",
        )),
    }
}

fn busy(error: &LedgerError, attempt: u32) -> bool {
    error.code == Code::Concurrency && attempt < CONCURRENCY_ATTEMPTS
}

async fn pause(attempt: u32) {
    tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt))).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn books() -> Books {
        Books {
            currency: "USD".into(),
            subsidiary_id: "1".into(),
            defaults: Defaults {
                expense_account_id: Some("65".into()),
                ..Defaults::default()
            },
            accounts: vec![AccountRec {
                external_id: "65".into(),
                inactive: false,
                requires: vec![],
            }],
            periods: vec![],
            vendor_currency: None,
        }
    }

    fn draft(memo: &str) -> BillDraft {
        BillDraft {
            bill_id: Uuid::from_u128(7),
            vendor_external_id: "v1".into(),
            invoice_number: "1001".into(),
            invoice_date: NaiveDate::from_ymd_opt(2027, 1, 15).unwrap(),
            due_date: None,
            memo: memo.into(),
            amount_cents: 1_250,
            lines: vec![],
        }
    }

    #[test]
    fn amounts_are_two_decimal_strings() {
        assert_eq!(cents_to_amount(1_250), "12.50");
        assert_eq!(cents_to_amount(-150), "-1.50");
        assert_eq!(cents_to_amount(-5), "-0.05");
        assert_eq!(cents_to_amount(0), "0.00");
    }

    #[test]
    fn a_changed_memo_changes_the_hash() {
        let books = books();
        let (_, first) = compose_bill(&books, &draft(""));
        let (_, second) = compose_bill(&books, &draft("changed"));
        assert_ne!(first, second);
    }

    #[test]
    fn a_discount_line_is_a_negative_amount() {
        let books = books();
        let mut draft = draft("");
        draft.lines = vec![
            DraftLine {
                account_id: "65".into(),
                amount_cents: 1_500,
                memo: "work".into(),
                department_id: None,
                class_id: None,
                location_id: None,
            },
            DraftLine {
                account_id: "65".into(),
                amount_cents: -250,
                memo: "discount".into(),
                department_id: None,
                class_id: None,
                location_id: None,
            },
        ];
        let (body, _) = compose_bill(&books, &draft);
        assert_eq!(body.lines[1].amount, "-2.50");
    }

    #[test]
    fn closed_period_missing_department_and_currency_do_not_build_a_body_call() {
        let mut books = books();
        books.periods.push(PeriodRec {
            start: NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2027, 1, 31).unwrap(),
            closed: true,
        });
        let PrepareError::Ledger(error) = plan_bill(&books, &draft(""), None).unwrap_err() else {
            panic!("closed period");
        };
        assert_eq!(error.code, Code::PeriodClosed);

        books.periods.clear();
        books.accounts[0].requires = vec!["department".into()];
        let PrepareError::Ledger(error) = plan_bill(&books, &draft(""), None).unwrap_err() else {
            panic!("department");
        };
        assert_eq!(error.code, Code::MissingField);
        assert!(error.detail.contains("department"), "{}", error.detail);

        books.accounts[0].requires.clear();
        books.currency = "EUR".into();
        let PrepareError::Ledger(error) = plan_bill(&books, &draft(""), None).unwrap_err() else {
            panic!("currency");
        };
        assert_eq!(error.code, Code::Currency);
    }

    #[tokio::test]
    async fn replay_returns_the_first_id_and_a_change_conflicts() {
        let fake = FakeLedger::new();
        let books = books();
        let first = submit_bill(&fake, &books, &draft("a"), None).await.unwrap();
        assert!(!first.replay);
        let again = submit_bill(
            &fake,
            &books,
            &draft("a"),
            Some(&LinkView {
                status: LinkStatus::Posted,
                external_id: Some(first.posted.external_id.clone()),
                external_key: first.posted.external_key.clone(),
                request_hash: Some(first.hash.clone()),
            }),
        )
        .await
        .unwrap();
        assert!(again.replay);
        assert_eq!(again.posted.external_id, first.posted.external_id);
        let PrepareError::Ledger(error) = submit_bill(
            &fake,
            &books,
            &draft("b"),
            Some(&LinkView {
                status: LinkStatus::Posted,
                external_id: Some(first.posted.external_id.clone()),
                external_key: first.posted.external_key.clone(),
                request_hash: Some(first.hash),
            }),
        )
        .await
        .unwrap_err() else {
            panic!("conflict");
        };
        assert_eq!(error.code, Code::Conflict);
        assert_eq!(error.detail, "This changed after it was posted");
        assert!(!error.write_link);
        assert_eq!(
            fake.calls(),
            vec![format!("post_bill {}", bill_key(Uuid::from_u128(7)))]
        );
    }

    #[tokio::test]
    async fn concurrency_is_retried_then_stopped() {
        let fake = FakeLedger::new();
        fake.script_err(LedgerError::new(Code::Concurrency, "busy"));
        let posted = submit_bill(&fake, &books(), &draft(""), None)
            .await
            .unwrap();
        assert_eq!(posted.posted.external_id, "fake-bill-1");
        assert_eq!(fake.calls().len(), 2);

        let fake = FakeLedger::new();
        for _ in 0..CONCURRENCY_ATTEMPTS {
            fake.script_err(LedgerError::new(Code::Concurrency, "busy"));
        }
        let PrepareError::Ledger(error) = submit_bill(&fake, &books(), &draft(""), None)
            .await
            .unwrap_err()
        else {
            panic!("exhausted");
        };
        assert_eq!(error.code, Code::Concurrency);
        assert_eq!(fake.calls().len(), CONCURRENCY_ATTEMPTS as usize);
    }

    #[tokio::test]
    async fn a_refused_payment_void_does_not_void_the_bill() {
        let fake = FakeLedger::new();
        fake.script_err(LedgerError::new(
            Code::Conflict,
            "the ledger refused the void",
        ));
        let error = void_posted(&fake, "fake-bill-1", &["fake-payment-1"])
            .await
            .unwrap_err();
        assert_eq!(error.payments_voided, 0);
        assert_eq!(
            fake.calls(),
            vec!["void_payment fake-payment-1".to_string()]
        );
    }

    #[test]
    fn payment_before_the_bill_and_without_a_bank_map() {
        let books = books();
        let draft = PaymentDraft {
            payment_id: Uuid::from_u128(9),
            amount_cents: 1_250,
            tran_date: NaiveDate::from_ymd_opt(2027, 1, 20).unwrap(),
            memo: "ach txn".into(),
            bank_account_id: String::new(),
            bill_external_id: String::new(),
            vendor_external_id: "v1".into(),
        };
        let PrepareError::Ledger(error) = plan_payment(&books, &draft, None).unwrap_err() else {
            panic!("bill");
        };
        assert_eq!(error.code, Code::BillNotPosted);
        let mapped = PaymentDraft {
            bill_external_id: "fake-bill-1".into(),
            ..draft
        };
        let PrepareError::Ledger(error) = plan_payment(&books, &mapped, None).unwrap_err() else {
            panic!("map");
        };
        assert_eq!(error.code, Code::PaymentUnmapped);
    }

    fn closed_january() -> PeriodRec {
        PeriodRec {
            start: NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2027, 1, 31).unwrap(),
            closed: true,
        }
    }

    fn posted_link(outcome: &PostedOutcome) -> LinkView {
        LinkView {
            status: LinkStatus::Posted,
            external_id: Some(outcome.posted.external_id.clone()),
            external_key: outcome.posted.external_key.clone(),
            request_hash: Some(outcome.hash.clone()),
        }
    }

    fn payment_draft(id: u128, bank: &str) -> PaymentDraft {
        PaymentDraft {
            payment_id: Uuid::from_u128(id),
            amount_cents: 1_250,
            tran_date: NaiveDate::from_ymd_opt(2027, 1, 20).unwrap(),
            memo: "ach".into(),
            bank_account_id: bank.into(),
            bill_external_id: "fake-bill-1".into(),
            vendor_external_id: "v1".into(),
        }
    }

    #[tokio::test]
    async fn a_closed_period_after_posting_still_replays() {
        let fake = FakeLedger::new();
        let mut books = books();
        let first = submit_bill(&fake, &books, &draft("a"), None).await.unwrap();
        books.periods.push(closed_january());
        let again = submit_bill(&fake, &books, &draft("a"), Some(&posted_link(&first)))
            .await
            .unwrap();
        assert!(again.replay);
        assert_eq!(again.posted.external_id, first.posted.external_id);

        books.periods.clear();
        books.accounts[0].inactive = true;
        let again = submit_bill(&fake, &books, &draft("a"), Some(&posted_link(&first)))
            .await
            .unwrap();
        assert!(again.replay);
        assert_eq!(fake.calls().len(), 1);

        let mut cleared = draft("a");
        cleared.vendor_external_id.clear();
        let PrepareError::Ledger(error) =
            submit_bill(&fake, &books, &cleared, Some(&posted_link(&first)))
                .await
                .unwrap_err()
        else {
            panic!("vendor cleared");
        };
        assert_eq!(error.code, Code::Conflict);
        assert_eq!(error.detail, "This changed after it was posted");
        assert!(!error.write_link);
        assert_eq!(fake.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_payment_in_a_closed_period_is_refused_before_a_call() {
        let fake = FakeLedger::new();
        let mut books = books();
        books.periods.push(closed_january());
        let PrepareError::Ledger(error) =
            plan_payment(&books, &payment_draft(9, "77"), None).unwrap_err()
        else {
            panic!("preview");
        };
        assert_eq!(error.code, Code::PeriodClosed);
        let PrepareError::Ledger(error) =
            submit_payment(&fake, &books, &payment_draft(9, "77"), None)
                .await
                .unwrap_err()
        else {
            panic!("post");
        };
        assert_eq!(error.code, Code::PeriodClosed);
        assert!(fake.calls().is_empty());
    }

    #[tokio::test]
    async fn a_posted_payment_replays_after_the_bank_map_is_gone() {
        let fake = FakeLedger::new();
        let books = books();
        let first = submit_payment(&fake, &books, &payment_draft(9, "77"), None)
            .await
            .unwrap();
        assert_eq!(first.posted.external_id, "fake-payment-1");
        let mut cleared = payment_draft(9, "");
        let again = submit_payment(&fake, &books, &cleared, Some(&posted_link(&first)))
            .await
            .unwrap();
        assert!(again.replay);
        assert_eq!(again.posted.external_id, first.posted.external_id);

        let mut closed = books.clone();
        closed.periods.push(closed_january());
        cleared.bank_account_id = "88".into();
        let again = submit_payment(&fake, &closed, &cleared, Some(&posted_link(&first)))
            .await
            .unwrap();
        assert!(again.replay);
        assert_eq!(fake.calls().len(), 1);

        let PrepareError::Ledger(error) =
            submit_payment(&fake, &books, &payment_draft(9, "88"), None)
                .await
                .unwrap_err()
        else {
            panic!("different body");
        };
        assert_eq!(error.code, Code::Conflict);
        assert!(error.detail.contains("different body"), "{}", error.detail);
        let same = submit_payment(&fake, &books, &payment_draft(9, "77"), None)
            .await
            .unwrap();
        assert_eq!(same.posted.external_id, first.posted.external_id);
        assert!(!same.replay);
    }

    #[tokio::test]
    async fn voiding_reverses_every_posted_payment() {
        let fake = FakeLedger::new();
        let bill = submit_bill(&fake, &books(), &draft("a"), None)
            .await
            .unwrap();
        let first = submit_payment(&fake, &books(), &payment_draft(9, "77"), None)
            .await
            .unwrap();
        let second = submit_payment(&fake, &books(), &payment_draft(10, "88"), None)
            .await
            .unwrap();
        let n = void_posted(
            &fake,
            &bill.posted.external_id,
            &[
                first.posted.external_id.as_str(),
                second.posted.external_id.as_str(),
            ],
        )
        .await
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            fake.calls().iter().skip(3).cloned().collect::<Vec<_>>(),
            vec![
                format!("void_payment {}", first.posted.external_id),
                format!("void_payment {}", second.posted.external_id),
                format!("void_bill {}", bill.posted.external_id),
            ]
        );
    }

    #[tokio::test]
    async fn a_later_payment_void_stops_before_the_bill() {
        let fake = FakeLedger::new();
        let bill = submit_bill(&fake, &books(), &draft("a"), None)
            .await
            .unwrap();
        let first = submit_payment(&fake, &books(), &payment_draft(9, "77"), None)
            .await
            .unwrap();
        let second = submit_payment(&fake, &books(), &payment_draft(10, "88"), None)
            .await
            .unwrap();
        fake.script_ok();
        fake.script_err(LedgerError::new(
            Code::Conflict,
            "the ledger refused the void",
        ));
        let error = void_posted(
            &fake,
            &bill.posted.external_id,
            &[
                first.posted.external_id.as_str(),
                second.posted.external_id.as_str(),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(error.payments_voided, 1);
        assert!(
            !fake
                .calls()
                .iter()
                .any(|call| call.starts_with("void_bill"))
        );
        let again = fake
            .void_payment(&first.posted.external_id)
            .await
            .unwrap_err();
        assert_eq!(again.code, Code::Conflict);
        assert!(again.detail.contains("already voided"), "{}", again.detail);
    }

    #[tokio::test]
    async fn the_fake_refuses_a_different_body_and_an_unknown_void() {
        let fake = FakeLedger::new();
        fake.script_err(LedgerError::new(Code::DuplicateInvoice, "dup"));
        let PrepareError::Ledger(error) = submit_bill(&fake, &books(), &draft("a"), None)
            .await
            .unwrap_err()
        else {
            panic!("scripted");
        };
        assert_eq!(error.code, Code::DuplicateInvoice);
        let posted = submit_bill(&fake, &books(), &draft("b"), None)
            .await
            .unwrap();
        assert_eq!(posted.posted.external_id, "fake-bill-1");

        let unknown = fake.void_bill("missing").await.unwrap_err();
        assert_eq!(unknown.code, Code::Conflict);
        assert!(unknown.detail.contains("unknown"), "{}", unknown.detail);
        fake.void_bill(&posted.posted.external_id).await.unwrap();
        let twice = fake
            .void_bill(&posted.posted.external_id)
            .await
            .unwrap_err();
        assert!(twice.detail.contains("already voided"), "{}", twice.detail);

        let file = fake
            .attach_file(&posted.posted.external_id, "invoice.pdf", b"one")
            .await
            .unwrap();
        let again = fake
            .attach_file(&posted.posted.external_id, "invoice.pdf", b"one")
            .await
            .unwrap();
        assert_eq!(file, again);
        let changed = fake
            .attach_file(&posted.posted.external_id, "invoice.pdf", b"two")
            .await
            .unwrap_err();
        assert_eq!(changed.code, Code::Conflict);
    }
}
