//! Paying a bill from the customer's own bank account.
//!
//! Payvy never holds or moves money. It asks the customer's bank, through
//! the bank's API and with the customer's own token, to pay a recipient the
//! customer already set up there. Bank account numbers stay in the bank:
//! Payvy keeps the recipient's id and shows people only the last four digits.
//!
//! Mercury is the first bank. Its two kinds of token map to two modes:
//!
//! - **request**: a Custom token with the `RequestSendMoney` scope. The
//!   payment waits in Mercury until a Mercury admin approves it there (not the
//!   person who made the token). No IP allowlist needed. The default.
//! - **direct**: a Read and Write token. Mercury sends at once. Mercury
//!   requires the server's IP on the token's allowlist.
//!
//! Ramp, Brex, Wise and Airwallex fit the same shape: list recipients, send
//! or request a payment under an idempotency key, read its status.
//!
//! Rho is the exception that proves the capability split: its API only
//! reads accounts and transactions, so its client below has no pay method
//! at all — a read-only connection can never pay by construction.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Ach,
    Check,
    DomesticWire,
    RealTimePayment,
}

impl Method {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(json!(s)).ok()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Method::Ach => "ach",
            Method::Check => "check",
            Method::DomesticWire => "domestic_wire",
            Method::RealTimePayment => "real_time_payment",
        }
    }

    pub fn mercury(self) -> &'static str {
        match self {
            Method::Ach => "ach",
            Method::Check => "check",
            Method::DomesticWire => "domesticWire",
            Method::RealTimePayment => "realTimePayment",
        }
    }
}

/// Where a payment stands at the bank, the same for every bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Waiting for someone to approve it inside the bank.
    AwaitingBankApproval,
    /// Accepted by the bank, not yet sent.
    Pending,
    Sent,
    Failed,
    Cancelled,
    /// A bank approver turned it down.
    Rejected,
    /// Sent, then came back (an ACH return).
    Returned,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::AwaitingBankApproval => "awaiting_bank_approval",
            Status::Pending => "pending",
            Status::Sent => "sent",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
            Status::Rejected => "rejected",
            Status::Returned => "returned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(json!(s)).ok()
    }

    /// Nothing more will happen to it without a person, except that a sent
    /// ACH can still come back as returned.
    pub fn is_final(self) -> bool {
        !matches!(self, Status::AwaitingBankApproval | Status::Pending)
    }

    /// The money didn't go (or came back): the bill needs paying again.
    pub fn did_not_pay(self) -> bool {
        matches!(
            self,
            Status::Failed | Status::Cancelled | Status::Rejected | Status::Returned
        )
    }

    fn from_mercury_transaction(s: &str) -> Self {
        match s {
            "pending" => Status::Pending,
            "sent" => Status::Sent,
            "cancelled" => Status::Cancelled,
            "reversed" => Status::Returned,
            // failed, blocked, and anything new: treat as not paid, so a
            // person looks at it rather than Payvy assuming it went.
            _ => Status::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Request,
    Direct,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(json!(s)).ok()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Request => "request",
            Mode::Direct => "direct",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BankError {
    #[error("the bank refused the token: {0}")]
    Unauthorized(String),
    /// The bank answered and did not create anything (a bad request, a
    /// duplicate it blocked, an unknown recipient). Safe to report as failed.
    #[error("the bank refused: {0}")]
    Refused(String),
    /// No clear answer (a timeout, a 5xx): the payment may or may not exist.
    /// Retry with the same idempotency key to find out.
    #[error("the bank didn't answer clearly: {0}")]
    Unclear(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub status: String,
    pub available_cents: i64,
    pub can_send_real_time: bool,
}

/// A payee as set up in the bank. Only the last four digits of an account
/// number ever leave this crate.
#[derive(Debug, Clone, Serialize)]
pub struct Recipient {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub emails: Vec<String>,
    /// The ways this recipient can be paid, with the account's last four.
    pub methods: Vec<RecipientMethod>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecipientMethod {
    pub method: Method,
    /// "…6789" for electronic methods; the city for a check.
    pub destination: String,
}

impl Recipient {
    pub fn accepts(&self, method: Method) -> bool {
        self.methods.iter().any(|m| m.method == method)
    }

    pub fn destination(&self, method: Method) -> Option<&str> {
        self.methods
            .iter()
            .find(|m| m.method == method)
            .map(|m| m.destination.as_str())
    }
}

pub struct Payment<'a> {
    pub account_id: &'a str,
    pub recipient_id: &'a str,
    pub amount_cents: i64,
    pub method: Method,
    /// The same key always means the same payment: a retry can't pay twice.
    pub idempotency_key: &'a str,
    /// Shown to the customer in the bank.
    pub note: &'a str,
    /// Shown to the vendor, e.g. the invoice number.
    pub memo: &'a str,
}

/// What the bank says about one payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observed {
    pub status: Status,
    /// The bank's transaction id, once there is one.
    pub transaction_id: Option<String>,
    /// The approval request's id, in request mode.
    pub request_id: Option<String>,
    pub failure: Option<String>,
    /// The link to it in the bank's own app.
    pub link: Option<String>,
}

/// Only the identifiers and lifecycle leave the client. In particular, never
/// expose the onboarding URL (a capability link), bank details or raw errors.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientInvite {
    pub id: String,
    pub contact_email: String,
    pub payment_methods: Vec<String>,
    pub status: String,
    pub recipient_id: Option<String>,
}

/// A legal address, as Mercury wants it on a recipient.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Address {
    pub address1: String,
    #[serde(default)]
    pub address2: Option<String>,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    #[serde(default = "us")]
    pub country: String,
}

fn us() -> String {
    "US".into()
}

/// A recipient to create from details a person typed. The numbers pass
/// straight through to Mercury and are never kept or logged.
pub struct NewRecipient<'a> {
    pub name: &'a str,
    pub email: &'a str,
    pub method: Method,
    pub account_number: &'a str,
    pub routing_number: &'a str,
    /// Mercury's electronicAccountType, for ACH (e.g. businessChecking).
    pub account_type: &'a str,
    pub address: &'a Address,
}

/// Replaces every run of four or more digits, so an error Mercury echoes
/// can't carry an account or routing number.
pub fn redact_numbers(s: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    for ch in s.chars().chain(std::iter::once('\0')) {
        if ch.is_ascii_digit() {
            run.push(ch);
            continue;
        }
        if run.len() >= 4 {
            out.push_str("[number]");
        } else {
            out.push_str(&run);
        }
        run.clear();
        if ch != '\0' {
            out.push(ch);
        }
    }
    out
}

/// A webhook endpoint registered at Mercury. The secret signs deliveries
/// and is returned only when the endpoint is created.
pub struct Webhook {
    pub id: String,
    pub secret: String,
}

pub struct Mercury {
    base: String,
    token: String,
    http: reqwest::Client,
}

pub const MERCURY_PRODUCTION: &str = "https://api.mercury.com/api/v1";
pub const MERCURY_SANDBOX: &str = "https://api-sandbox.mercury.com/api/v1";

fn cents(amount: &Value) -> i64 {
    (amount.as_f64().unwrap_or(0.0).abs() * 100.0).round() as i64
}

fn last4(number: &str) -> String {
    let digits: Vec<char> = number.chars().filter(char::is_ascii_alphanumeric).collect();
    let tail: String = digits[digits.len().saturating_sub(4)..].iter().collect();
    format!("…{tail}")
}

fn observed_transaction(t: &Value) -> Observed {
    Observed {
        status: Status::from_mercury_transaction(t["status"].as_str().unwrap_or("")),
        transaction_id: t["id"].as_str().map(str::to_owned),
        request_id: t["requestId"].as_str().map(str::to_owned),
        failure: t["reasonForFailure"].as_str().map(str::to_owned),
        link: t["dashboardLink"].as_str().map(str::to_owned),
    }
}

fn observed_request(r: &Value) -> Observed {
    let status = match r["status"].as_str().unwrap_or("") {
        "pendingApproval" => Status::AwaitingBankApproval,
        // Approved: Mercury now makes the transaction; it shows up under
        // the request's id.
        "approved" => Status::Pending,
        "rejected" => Status::Rejected,
        _ => Status::Cancelled,
    };
    Observed {
        status,
        transaction_id: None,
        request_id: r["requestId"].as_str().map(str::to_owned),
        failure: (status == Status::Rejected).then(|| "rejected by an approver in Mercury".into()),
        link: None,
    }
}

impl Mercury {
    pub fn new(base: &str, token: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.trim().to_owned(),
            http,
        })
    }

    async fn call(&self, req: reqwest::RequestBuilder) -> Result<(u16, Value), BankError> {
        let res = req
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| BankError::Unclear(format!("couldn't reach Mercury: {e}")))?;
        let status = res.status().as_u16();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    fn error(status: u16, body: &Value) -> BankError {
        let message = body["errors"]["message"]
            .as_str()
            .or_else(|| body["message"].as_str())
            .or_else(|| body["error"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {status}"));
        match status {
            401 | 403 => BankError::Unauthorized(message),
            400..=499 => BankError::Refused(message),
            _ => BankError::Unclear(message),
        }
    }

    async fn get(&self, path: &str) -> Result<Value, BankError> {
        let (status, body) = self
            .call(self.http.get(format!("{}{path}", self.base)))
            .await?;
        if (200..300).contains(&status) {
            Ok(body)
        } else {
            Err(Self::error(status, &body))
        }
    }

    async fn invite_call(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<RecipientInvite, BankError> {
        // Bank errors may echo input or capability links. Keep the category,
        // but never forward their body (or reqwest URLs) to logs or callers.
        let (status, body) = self.call(req).await.map_err(|_| {
            BankError::Unclear(
                "couldn't reach Mercury; check the invitation in Mercury before trying again"
                    .into(),
            )
        })?;
        if !(200..300).contains(&status) {
            return Err(Self::error(
                status,
                &json!({ "message": format!("Mercury invite HTTP {status}") }),
            ));
        }
        let invite: RecipientInvite = serde_json::from_value(body)
            .map_err(|_| BankError::Unclear("Mercury returned an incomplete invitation".into()))?;
        if invite.id.trim().is_empty()
            || !matches!(
                invite.status.as_str(),
                "created" | "completed" | "expired" | "declined"
            )
        {
            return Err(BankError::Unclear(
                "Mercury returned an unrecognized invitation".into(),
            ));
        }
        Ok(invite)
    }

    /// Mercury sends the email; the vendor enters payment details there.
    /// This endpoint has no documented idempotency key: never retry a POST.
    pub async fn invite_recipient(
        &self,
        name: &str,
        email: &str,
        method: Method,
    ) -> Result<RecipientInvite, BankError> {
        self.invite_call(
            self.http
                .post(format!("{}/recipients/invites", self.base))
                .json(&json!({
                    "name": name, "contactEmail": email, "paymentMethods": [method.mercury()],
                    "requireTaxDocument": false, "sendEmail": true
                })),
        )
        .await
    }

    pub async fn recipient_invite(&self, id: &str) -> Result<RecipientInvite, BankError> {
        let mut url = reqwest::Url::parse(&format!("{}/recipients/invites/", self.base))
            .map_err(|_| BankError::Refused("invalid Mercury URL".into()))?;
        url.path_segments_mut()
            .map_err(|_| BankError::Refused("invalid Mercury URL".into()))?
            .pop_if_empty()
            .push(id);
        self.invite_call(self.http.get(url)).await
    }

    pub async fn accounts(&self) -> Result<Vec<Account>, BankError> {
        let body = self.get("/accounts").await?;
        Ok(body["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| Account {
                id: a["id"].as_str().unwrap_or_default().to_owned(),
                name: a["name"].as_str().unwrap_or_default().to_owned(),
                status: a["status"].as_str().unwrap_or_default().to_owned(),
                available_cents: cents(&a["availableBalance"]),
                can_send_real_time: a["canSendRealTimePayments"].as_bool().unwrap_or(false),
            })
            .collect())
    }

    fn recipient(r: &Value) -> Recipient {
        let mut methods = Vec::new();
        for (key, method) in [
            ("electronicRoutingInfo", Method::Ach),
            ("domesticWireRoutingInfo", Method::DomesticWire),
            ("realTimePaymentRoutingInfo", Method::RealTimePayment),
        ] {
            if let Some(n) = r[key]["accountNumber"].as_str() {
                methods.push(RecipientMethod {
                    method,
                    destination: last4(n),
                });
            }
        }
        if r["checkInfo"]["address"].is_object() {
            let a = &r["checkInfo"]["address"];
            methods.push(RecipientMethod {
                method: Method::Check,
                destination: format!(
                    "check to {}, {}",
                    a["city"].as_str().unwrap_or("?"),
                    a["region"].as_str().unwrap_or("?")
                ),
            });
        }
        Recipient {
            id: r["id"].as_str().unwrap_or_default().to_owned(),
            name: r["name"].as_str().unwrap_or_default().to_owned(),
            active: r["status"] == "active",
            emails: r["emails"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e.as_str().map(str::to_owned))
                .collect(),
            methods,
        }
    }

    /// Every recipient, following Mercury's pages.
    pub async fn recipients(&self) -> Result<Vec<Recipient>, BankError> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let path = match &after {
                Some(a) => format!("/recipients?limit=1000&start_after={a}"),
                None => "/recipients?limit=1000".to_owned(),
            };
            let body = self.get(&path).await?;
            let page: Vec<Recipient> = body["recipients"]
                .as_array()
                .into_iter()
                .flatten()
                .map(Self::recipient)
                .collect();
            let next = body["page"]["nextPage"].as_str().map(str::to_owned);
            let empty = page.is_empty();
            out.extend(page);
            match next {
                Some(n) if !empty && after.as_deref() != Some(n.as_str()) => after = Some(n),
                _ => return Ok(out),
            }
        }
    }

    pub async fn recipient_by_id(&self, id: &str) -> Result<Option<Recipient>, BankError> {
        match self.get(&format!("/recipient/{id}")).await {
            Ok(r) => Ok(Some(Self::recipient(&r))),
            Err(BankError::Refused(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Sends (direct) or asks to send (request). Retrying with the same
    /// idempotency key returns the payment already made instead of a second.
    pub async fn pay(&self, mode: Mode, p: &Payment<'_>) -> Result<Observed, BankError> {
        let amount = serde_json::Number::from_f64(p.amount_cents as f64 / 100.0)
            .ok_or_else(|| BankError::Refused("amount isn't a number".into()))?;
        let mut body = json!({
            "recipientId": p.recipient_id,
            "amount": amount,
            "paymentMethod": p.method.mercury(),
            "idempotencyKey": p.idempotency_key,
            "note": p.note,
            "externalMemo": p.memo,
        });
        if p.method == Method::DomesticWire {
            body["purpose"] =
                json!({ "simple": { "category": "vendor", "additionalInfo": p.memo } });
        }
        let path = match mode {
            Mode::Direct => format!("/account/{}/transactions", p.account_id),
            Mode::Request => format!("/account/{}/request-send-money", p.account_id),
        };
        let (status, body) = self
            .call(self.http.post(format!("{}{path}", self.base)).json(&body))
            .await?;
        // 409: this key was used already; the body is what it made.
        if (200..300).contains(&status) || status == 409 {
            return Ok(match mode {
                Mode::Direct => observed_transaction(&body),
                Mode::Request => observed_request(&body),
            });
        }
        Err(Self::error(status, &body))
    }

    /// Creates a recipient. Mercury has no idempotency key here, so a POST is
    /// never retried: an unclear answer means "look in Mercury first".
    /// Errors never carry Mercury's body unredacted.
    pub async fn create_recipient(&self, r: &NewRecipient<'_>) -> Result<Recipient, BankError> {
        let a = r.address;
        let address = json!({
            "address1": a.address1, "address2": a.address2, "city": a.city,
            "region": a.region, "postalCode": a.postal_code, "country": a.country,
        });
        let mut body = json!({ "name": r.name, "emails": [r.email], "contactEmail": r.email });
        let routing = json!({ "accountNumber": r.account_number, "routingNumber": r.routing_number,
                              "address": address });
        match r.method {
            Method::Check => body["checkInfo"] = json!({ "address": address }),
            Method::Ach => {
                let mut info = routing;
                info["electronicAccountType"] = json!(r.account_type);
                body["electronicRoutingInfo"] = info;
            }
            Method::DomesticWire => body["domesticWireRoutingInfo"] = routing,
            Method::RealTimePayment => body["realTimePaymentRoutingInfo"] = routing,
        }
        let (status, reply) = self
            .call(
                self.http
                    .post(format!("{}/recipients", self.base))
                    .json(&body),
            )
            .await
            .map_err(|_| {
                BankError::Unclear(
                    "couldn't reach Mercury; check Mercury's recipients before trying again".into(),
                )
            })?;
        if !(200..300).contains(&status) {
            let e = Self::error(status, &reply);
            return Err(match e {
                BankError::Refused(m) => BankError::Refused(redact_numbers(&m)),
                BankError::Unauthorized(m) => BankError::Unauthorized(redact_numbers(&m)),
                BankError::Unclear(_) => BankError::Unclear(format!(
                    "Mercury answered HTTP {status}; check Mercury's recipients before trying again"
                )),
            });
        }
        let made = Self::recipient(&reply);
        if made.id.is_empty() {
            return Err(BankError::Unclear(
                "Mercury didn't return the new recipient; check Mercury's recipients before trying again".into(),
            ));
        }
        Ok(made)
    }

    /// Registers `url` for transaction events. Mercury returns the signing
    /// secret only here.
    pub async fn create_webhook(&self, url: &str) -> Result<Webhook, BankError> {
        let (status, body) = self
            .call(
                self.http
                    .post(format!("{}/webhooks", self.base))
                    .json(&json!({
                        "url": url, "eventTypes": ["transaction.created", "transaction.updated"]
                    })),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(Self::error(status, &body));
        }
        match (body["id"].as_str(), body["secret"].as_str()) {
            (Some(id), Some(secret)) if !id.is_empty() && !secret.is_empty() => Ok(Webhook {
                id: id.to_owned(),
                secret: secret.to_owned(),
            }),
            _ => Err(BankError::Unclear(
                "Mercury didn't return the webhook's id and secret".into(),
            )),
        }
    }

    /// Removes a webhook endpoint. One that's already gone counts as removed.
    pub async fn delete_webhook(&self, id: &str) -> Result<(), BankError> {
        let mut url = reqwest::Url::parse(&format!("{}/webhooks/", self.base))
            .map_err(|_| BankError::Refused("invalid Mercury URL".into()))?;
        url.path_segments_mut()
            .map_err(|_| BankError::Refused("invalid Mercury URL".into()))?
            .pop_if_empty()
            .push(id);
        let (status, body) = self.call(self.http.delete(url)).await?;
        if (200..300).contains(&status) || status == 404 {
            Ok(())
        } else {
            Err(Self::error(status, &body))
        }
    }

    /// Where a payment stands now. In request mode, once approved, the
    /// transaction is found by the request's id.
    pub async fn status(
        &self,
        account_id: &str,
        transaction_id: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<Observed, BankError> {
        if let Some(t) = transaction_id {
            let mut o = observed_transaction(
                &self
                    .get(&format!("/account/{account_id}/transaction/{t}"))
                    .await?,
            );
            o.request_id = o.request_id.or(request_id.map(str::to_owned));
            return Ok(o);
        }
        let Some(r) = request_id else {
            return Err(BankError::Refused(
                "nothing to look up: no transaction or request id".into(),
            ));
        };
        let request = observed_request(&self.get(&format!("/request-send-money/{r}")).await?);
        if request.status != Status::Pending {
            return Ok(request);
        }
        let found = self
            .get(&format!(
                "/account/{account_id}/transactions?requestId={r}&limit=10"
            ))
            .await?;
        Ok(
            match found["transactions"].as_array().and_then(|t| t.first()) {
                Some(t) => {
                    let mut o = observed_transaction(t);
                    o.request_id = Some(r.to_owned());
                    o
                }
                // Approved, transaction not made yet.
                None => request,
            },
        )
    }
}

/// One Rho transaction. Amounts are signed integer minor units at Rho
/// (negative = money out), and Payvy keeps that exact value — no float.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RhoTransaction {
    pub id: String,
    pub account_id: String,
    pub account_name: String,
    pub transaction_type: String,
    pub status: String,
    /// Signed cents: negative for an outflow, positive for an inflow.
    pub amount_cents: i64,
    pub currency: String,
    pub counterparty_name: String,
    pub initiated_at: String,
    pub posted_at: Option<String>,
    pub memo: Option<String>,
    pub note: Option<String>,
    pub money_movement_id: Option<String>,
}

/// Filters for `Rho::transactions`. Times are ISO 8601, passed through.
#[derive(Debug, Clone, Default)]
pub struct RhoTransactionQuery {
    pub account_id: Option<String>,
    pub posted_after: Option<String>,
    pub posted_before: Option<String>,
    pub initiated_after: Option<String>,
    pub initiated_before: Option<String>,
    pub search: Option<String>,
    /// Stop after this many transactions (pages of 100 underneath).
    pub limit: usize,
}

/// Rho (rho.co): a read-only bank connection. Its API v1 covers accounts
/// and transactions only, so this client can look and can never pay:
/// there is no pay method to call by mistake.
pub struct Rho {
    base: String,
    token: String,
    http: reqwest::Client,
}

pub const RHO_PRODUCTION: &str = "https://rhoapi.rho.co/api/v1";
pub const RHO_SANDBOX: &str = "https://rhoapi-sandbox.rho.co/api/v1";

impl Rho {
    pub fn new(base: &str, token: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.trim().to_owned(),
            http,
        })
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, BankError> {
        let res = self
            .http
            .get(format!("{}{path}", self.base))
            .query(query)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| BankError::Unclear(format!("couldn't reach Rho: {e}")))?;
        let status = res.status().as_u16();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(body);
        }
        // Rho errors are RFC 9457 problem+json: detail, else title.
        let message = body["detail"]
            .as_str()
            .or_else(|| body["title"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(match status {
            401 | 403 => BankError::Unauthorized(message),
            400..=499 => BankError::Refused(message),
            _ => BankError::Unclear(message),
        })
    }

    fn account(a: &Value) -> Account {
        Account {
            id: a["id"].as_str().unwrap_or_default().to_owned(),
            name: a["account_name"].as_str().unwrap_or_default().to_owned(),
            // Rho's account shape has no status field; a listed account
            // is readable. Rho never sends, so this pays no one either way.
            status: "active".into(),
            available_cents: a["balance"]["amount"].as_i64().unwrap_or(0),
            can_send_real_time: false,
        }
    }

    fn transaction(t: &Value) -> RhoTransaction {
        RhoTransaction {
            id: t["id"].as_str().unwrap_or_default().to_owned(),
            account_id: t["account_id"].as_str().unwrap_or_default().to_owned(),
            account_name: t["account_name"].as_str().unwrap_or_default().to_owned(),
            transaction_type: t["transaction_type"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            status: t["status"].as_str().unwrap_or_default().to_owned(),
            amount_cents: t["amount"]["amount"].as_i64().unwrap_or(0),
            currency: t["amount"]["currency"].as_str().unwrap_or("USD").to_owned(),
            counterparty_name: t["counterparty_name"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            initiated_at: t["initiated_at"].as_str().unwrap_or_default().to_owned(),
            posted_at: t["posted_at"].as_str().map(str::to_owned),
            memo: t["memo"].as_str().map(str::to_owned),
            note: t["note"].as_str().map(str::to_owned),
            money_movement_id: t["money_movement_id"].as_str().map(str::to_owned),
        }
    }

    /// Every account, following Rho's cursor pages.
    pub async fn accounts(&self) -> Result<Vec<Account>, BankError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..50 {
            let mut query: Vec<(&str, String)> = vec![("page_size", "100".into())];
            if let Some(t) = &token {
                query.push(("page_token", t.clone()));
            }
            let body = self.get("/accounts", &query).await?;
            out.extend(
                body["accounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(Self::account),
            );
            match body["page"]["next_page_token"].as_str() {
                Some(next) if !next.is_empty() => token = Some(next.to_owned()),
                _ => return Ok(out),
            }
        }
        Ok(out)
    }

    /// Transactions matching the query, following cursor pages until the
    /// limit is met or Rho runs out.
    pub async fn transactions(
        &self,
        q: &RhoTransactionQuery,
    ) -> Result<Vec<RhoTransaction>, BankError> {
        let limit = if q.limit == 0 { 100 } else { q.limit.min(1000) };
        let mut out: Vec<RhoTransaction> = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..50 {
            let mut query: Vec<(&str, String)> = vec![("page_size", "100".into())];
            if let Some(v) = &q.account_id {
                query.push(("account_id", v.clone()));
            }
            if let Some(v) = &q.posted_after {
                query.push(("posted_after", v.clone()));
            }
            if let Some(v) = &q.posted_before {
                query.push(("posted_before", v.clone()));
            }
            if let Some(v) = &q.initiated_after {
                query.push(("initiated_after", v.clone()));
            }
            if let Some(v) = &q.initiated_before {
                query.push(("initiated_before", v.clone()));
            }
            if let Some(v) = &q.search {
                query.push(("search", v.clone()));
            }
            if let Some(t) = &token {
                query.push(("page_token", t.clone()));
            }
            let body = self.get("/transactions", &query).await?;
            out.extend(
                body["transactions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(Self::transaction),
            );
            if out.len() >= limit {
                out.truncate(limit);
                return Ok(out);
            }
            match body["page"]["next_page_token"].as_str() {
                Some(next) if !next.is_empty() => token = Some(next.to_owned()),
                _ => return Ok(out),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipients_keep_only_the_last_four() {
        let r = Mercury::recipient(&json!({
            "id": "r1", "name": "King Environmental", "status": "active", "emails": ["ar@kingenv.com"],
            "electronicRoutingInfo": { "accountNumber": "000123456789", "routingNumber": "121000248" },
            "checkInfo": { "address": { "address1": "PO Box 4410", "city": "Mesa", "region": "AZ" } }
        }));
        assert_eq!(r.destination(Method::Ach), Some("…6789"));
        assert_eq!(r.destination(Method::Check), Some("check to Mesa, AZ"));
        assert!(!r.accepts(Method::DomesticWire));
        let shown = serde_json::to_string(&r).unwrap();
        assert!(!shown.contains("123456789") && !shown.contains("121000248"));
    }

    #[test]
    fn mercury_statuses_map_conservatively() {
        let t = |s: &str| observed_transaction(&json!({ "id": "t", "status": s })).status;
        assert_eq!(t("pending"), Status::Pending);
        assert_eq!(t("sent"), Status::Sent);
        assert_eq!(t("reversed"), Status::Returned);
        assert_eq!(t("blocked"), Status::Failed);
        assert_eq!(t("something-new"), Status::Failed);
        let r = |s: &str| observed_request(&json!({ "requestId": "q", "status": s })).status;
        assert_eq!(r("pendingApproval"), Status::AwaitingBankApproval);
        assert_eq!(r("approved"), Status::Pending);
        assert_eq!(r("rejected"), Status::Rejected);
        assert!(Status::Returned.did_not_pay() && Status::Sent.is_final());
        assert!(!Status::Pending.is_final());
    }

    #[test]
    fn rho_amounts_are_signed_minor_units() {
        let a = Rho::account(&json!({
            "id": "acc-1", "account_name": "Operating", "account_type": "checking",
            "balance": { "amount": 5200050, "currency": "USD" }, "account_number_last_4": "6789"
        }));
        assert_eq!(a.id, "acc-1");
        assert_eq!(a.name, "Operating");
        assert_eq!(a.available_cents, 5_200_050);
        assert!(!a.can_send_real_time);
        let shown = serde_json::to_string(&a).unwrap();
        assert!(
            !shown.contains("6789"),
            "account numbers never surface: {shown}"
        );
        let t = Rho::transaction(&json!({
            "id": "txn-1", "account_id": "acc-1", "account_name": "Operating",
            "transaction_type": "ach_debit", "status": "settled",
            "amount": { "amount": -245000, "currency": "USD" },
            "counterparty_name": "Northline Supply", "initiated_at": "2026-11-01T09:00:00Z",
            "posted_at": "2026-11-03T09:00:00Z", "memo": "INV-1042", "money_movement_id": "mm-1"
        }));
        assert_eq!(t.amount_cents, -245_000);
        assert_eq!(t.counterparty_name, "Northline Supply");
        assert_eq!(t.posted_at.as_deref(), Some("2026-11-03T09:00:00Z"));
        let credit = Rho::transaction(&json!({
            "id": "txn-2", "account_id": "acc-1", "transaction_type": "ach_credit", "status": "pending",
            "amount": { "amount": 100, "currency": "USD" }, "initiated_at": "2026-11-04T09:00:00Z"
        }));
        assert_eq!(credit.amount_cents, 100);
        assert_eq!(credit.posted_at, None);
    }

    #[test]
    fn amounts_are_dollars_on_the_wire() {
        assert_eq!(cents(&json!(-1234.56)), 123_456);
        assert_eq!(cents(&json!(0.1)), 10);
        let n = serde_json::Number::from_f64(45_900_f64 / 100.0).unwrap();
        assert_eq!(n.to_string(), "459.0");
        let n = serde_json::Number::from_f64(123_456_f64 / 100.0).unwrap();
        assert_eq!(n.to_string(), "1234.56");
    }
}

#[cfg(test)]
mod redact_tests {
    use super::redact_numbers;

    #[test]
    fn long_digit_runs_are_hidden() {
        assert_eq!(
            redact_numbers("routing 121000248 bad, account 000123456789; field 12"),
            "routing [number] bad, account [number]; field 12"
        );
        assert_eq!(redact_numbers("4410"), "[number]");
    }
}
