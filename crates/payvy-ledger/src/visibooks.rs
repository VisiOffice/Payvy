//! VisiBooks recording transport. No payment rail, credential logging, or redirects.
use crate::{
    BillBody, Catalog, CatalogAccount, CatalogPeriod, CatalogSegment, CatalogVendor, Code,
    LEDGER_CALL_TIMEOUT, Ledger, LedgerError, PaymentBody, Posted,
};
use chrono::NaiveDate;
use reqwest::{Client, Method, Url, header};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

type Call<'a, T> = Pin<Box<dyn Future<Output = Result<T, LedgerError>> + Send + 'a>>;

pub struct VisiBooks {
    client: Client,
    base: Url,
    entity: String,
}

impl VisiBooks {
    /// HTTPS origin only. Credentials are installed as a sensitive header and
    /// never exposed through Debug or provider error messages.
    pub fn new(origin: &str, entity: &str, token: &str) -> Result<Self, LedgerError> {
        Self::build(origin, entity, token, false)
    }

    /// A loopback origin over plain HTTP, for development and tests
    /// against a local VisiAPI. Refuses anything that isn't loopback.
    pub fn new_local(origin: &str, entity: &str, token: &str) -> Result<Self, LedgerError> {
        Self::build(origin, entity, token, true)
    }

    fn build(
        origin: &str,
        entity: &str,
        token: &str,
        local_test: bool,
    ) -> Result<Self, LedgerError> {
        let base = Url::parse(origin).map_err(|_| missing("Invalid VisiBooks origin"))?;
        let loopback =
            local_test && base.scheme() == "http" && base.host_str() == Some("127.0.0.1");
        if (base.scheme() != "https" && !loopback)
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err(missing(
                "VisiBooks requires an HTTPS origin without credentials or a path",
            ));
        }
        id(entity)?;
        if !token.starts_with("vk_") || token.trim() != token {
            return Err(LedgerError::new(
                Code::NeedsAuth,
                "Provide a VisiBooks API key",
            ));
        }
        let mut headers = header::HeaderMap::new();
        let mut auth = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| LedgerError::new(Code::NeedsAuth, "Invalid VisiBooks API key"))?;
        auth.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, auth);
        headers.insert(
            "x-books-entity-id",
            header::HeaderValue::from_str(entity).map_err(|_| missing("Invalid entity"))?,
        );
        let client = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(LEDGER_CALL_TIMEOUT)
            .build()
            .map_err(|_| missing("Could not configure VisiBooks client"))?;
        Ok(Self {
            client,
            base,
            entity: entity.into(),
        })
    }

    async fn request(&self, method: Method, path: &str, body: Value) -> Result<Value, LedgerError> {
        self.send(method, path, Some(body), 65_536).await
    }

    /// One page of a read. Pages are larger than post answers (up to 100
    /// rows), so the cap is 1 MiB; it is still a cap.
    async fn read(&self, path: &str) -> Result<Value, LedgerError> {
        self.send(Method::GET, path, None, 1_048_576).await
    }

    /// Every row of a list, following `page.has_more` with `after=<last id>`.
    /// Ids must strictly increase, so a server that repeats a page can't
    /// loop forever; at most 500 pages are read.
    async fn read_all(&self, path: &str) -> Result<(Vec<Value>, Value), LedgerError> {
        let mut rows = Vec::new();
        let mut first = Value::Null;
        let mut after: Option<i64> = None;
        let joiner = if path.contains('?') { '&' } else { '?' };
        for _ in 0..500 {
            let url = match after {
                Some(id) => format!("{path}{joiner}limit=100&after={id}"),
                None => format!("{path}{joiner}limit=100"),
            };
            let page = self.read(&url).await?;
            let data = page
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| unreadable("a list without data"))?;
            for row in data {
                let id = row
                    .get("id")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| unreadable("a row without an id"))?;
                if after.is_some_and(|last| id <= last) {
                    return Err(unreadable("ids that don't increase"));
                }
                after = Some(id);
                rows.push(row.clone());
            }
            if first.is_null() {
                first = page.clone();
            }
            let more = page
                .pointer("/page/has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !more || data.is_empty() {
                return Ok((rows, first));
            }
        }
        Err(unreadable("more than 500 pages"))
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        cap: usize,
    ) -> Result<Value, LedgerError> {
        let url = self
            .base
            .join(path)
            .map_err(|_| missing("Invalid ledger path"))?;
        let mut request = self.client.request(method, url);
        if let Some(body) = &body {
            request = request.json(body);
        }
        let mut response = request.send().await.map_err(transport_error)?;
        let status = response.status();
        // Never buffer arbitrary upstream pages (or reflect their content into
        // the bill's history). Large or malformed answers leave the same key retryable.
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if bytes.len() + chunk.len() > cap {
                return Err(LedgerError::new(
                    Code::Timeout,
                    "VisiBooks response was too large; retry the same request",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if status.is_redirection() {
            return Err(LedgerError::new(
                Code::NeedsAuth,
                "VisiBooks redirected the request; check its origin",
            ));
        }
        let parsed = serde_json::from_slice::<Value>(&bytes).ok();
        if !status.is_success() {
            return Err(response_error(
                status.as_u16(),
                parsed
                    .as_ref()
                    .and_then(|v| v.pointer("/error/code"))
                    .and_then(Value::as_str),
            ));
        }
        parsed.ok_or_else(|| {
            LedgerError::new(
                Code::Timeout,
                "VisiBooks returned an unreadable response; retry the same request",
            )
        })
    }

    fn check_entity(&self, entity: &str) -> Result<(), LedgerError> {
        if entity != self.entity {
            return Err(missing("Ledger entity does not match this connection"));
        }
        Ok(())
    }

    async fn posted(&self, path: &str, body: Value, key: &str) -> Result<Posted, LedgerError> {
        let result = self.request(Method::PUT, path, body).await?;
        if result.pointer("/data/external_id").and_then(Value::as_str) != Some(key) {
            return Err(LedgerError::new(
                Code::Conflict,
                "VisiBooks returned a different external key",
            ));
        }
        let external_id = result
            .pointer("/data/id")
            .and_then(Value::as_i64)
            .filter(|v| *v > 0)
            .ok_or_else(|| {
                LedgerError::new(
                    Code::Timeout,
                    "VisiBooks did not return a record ID; retry the same request",
                )
            })?;
        match result.pointer("/data/status").and_then(Value::as_str) {
            Some("posted" | "matched" | "approved" | "partially_paid" | "closed" | "confirmed") => {
            }
            _ => {
                return Err(LedgerError::new(
                    Code::Conflict,
                    "VisiBooks record is not posted or has been voided",
                ));
            }
        }
        Ok(Posted {
            external_id: external_id.to_string(),
            external_key: key.into(),
        })
    }

    async fn void_record(&self, kind: &str, external_id: &str) -> Result<(), LedgerError> {
        let record_id = id(external_id)?;
        let result = self
            .request(
                Method::POST,
                &format!("/api/v1/books/payvy/{kind}/{record_id}/void"),
                json!({}),
            )
            .await?;
        if result.pointer("/data/id").and_then(Value::as_i64) != Some(record_id)
            || result.pointer("/data/status").and_then(Value::as_str) != Some("void")
        {
            return Err(LedgerError::new(
                Code::Conflict,
                "VisiBooks did not confirm the requested void",
            ));
        }
        Ok(())
    }
}

impl Ledger for VisiBooks {
    fn post_bill<'a>(&'a self, body: &'a BillBody) -> Call<'a, Posted> {
        Box::pin(async move {
            self.check_entity(&body.subsidiary_id)?;
            key(&body.external_key)?;
            // VisiBooks requires a date; an undated invoice is due on its
            // invoice date for this ledger export, without editing Payvy's bill.
            let due = body.due_date.as_ref().unwrap_or(&body.invoice_date);
            // Posted at payment, the bill carries the payment date.
            let issue = body.post_date.as_ref().unwrap_or(&body.invoice_date);
            let mut lines = Vec::new();
            for line in &body.lines {
                // Configured dimensions must name actual VisiBooks segment IDs.
                // Native bill lines currently carry one segment. Never discard
                // a second dimension or infer a segment from an arbitrary label.
                let segments: Vec<_> = [&line.department_id, &line.class_id, &line.location_id]
                    .into_iter()
                    .flatten()
                    .collect();
                if segments.len() > 1 {
                    return Err(missing(
                        "VisiBooks currently supports one segment per bill line",
                    ));
                }
                let mut value = json!({ "account_id": id(&line.account_id)?, "amount_cents": cents(&line.amount)?, "description": line.memo });
                if let Some(segment) = segments.first() {
                    value["segment_id"] = json!(id(segment)?);
                }
                lines.push(value);
            }
            let mut bill = json!({ "contact_id": id(&body.vendor_external_id)?, "vendor_invoice_number": body.invoice_number,
                "issue_date": issue, "due_date": due, "currency": body.currency, "notes": body.memo, "lines": lines });
            if let Some(period) = &body.service_period {
                bill["service_period"] =
                    json!({ "starts_on": period.starts_on, "ends_on": period.ends_on });
            }
            let request = json!({ "bill": bill });
            self.posted(
                &format!(
                    "/api/v1/books/bills/by_external/payvy/{}",
                    body.external_key
                ),
                request,
                &body.external_key,
            )
            .await
        })
    }

    fn post_payment<'a>(&'a self, body: &'a PaymentBody) -> Call<'a, Posted> {
        Box::pin(async move {
            self.check_entity(&body.subsidiary_id)?;
            key(&body.external_key)?;
            let method = match body.memo.as_str() {
                "ach" => "ach",
                "wire" | "domestic_wire" => "wire",
                "check" => "check",
                "card" => "card",
                "real_time_payment" | "other" => "other",
                _ => return Err(missing("Select a supported payment method")),
            };
            let amount = cents(&body.amount)?;
            let request = json!({ "payment": { "contact_id": id(&body.vendor_external_id)?, "bank_account_id": id(&body.account_id)?,
                "method": method, "paid_on": body.tran_date, "currency": body.currency, "amount_cents": amount,
                "applications": [{ "bill_id": id(&body.bill_external_id)?, "amount_cents": amount }] }});
            self.posted(
                &format!(
                    "/api/v1/books/vendor_payments/by_external/payvy/{}",
                    body.external_key
                ),
                request,
                &body.external_key,
            )
            .await
        })
    }

    fn set_document_link<'a>(&'a self, bill_external_key: &'a str, url: &'a str) -> Call<'a, ()> {
        Box::pin(async move {
            key(bill_external_key)?;
            self.request(
                Method::PUT,
                &format!("/api/v1/books/bills/by_external/payvy/{bill_external_key}/document_link"),
                json!({ "document_link": url }),
            )
            .await?;
            Ok(())
        })
    }

    fn set_bank_reference<'a>(
        &'a self,
        payment_external_key: &'a str,
        reference: &'a str,
    ) -> Call<'a, ()> {
        Box::pin(async move {
            key(payment_external_key)?;
            self.request(
                Method::PUT,
                &format!("/api/v1/books/vendor_payments/by_external/payvy/{payment_external_key}/bank_reference"),
                json!({ "bank_reference": reference }),
            )
            .await?;
            Ok(())
        })
    }

    fn void_bill<'a>(&'a self, external_id: &'a str) -> Call<'a, ()> {
        Box::pin(self.void_record("bills", external_id))
    }
    fn void_payment<'a>(&'a self, external_id: &'a str) -> Call<'a, ()> {
        Box::pin(self.void_record("vendor_payments", external_id))
    }
    fn attach_file<'a>(
        &'a self,
        _bill: &'a str,
        _name: &'a str,
        _bytes: &'a [u8],
    ) -> Call<'a, String> {
        Box::pin(async { Err(missing("VisiBooks file attachment is not implemented yet")) })
    }

    fn catalog<'a>(&'a self) -> Call<'a, Catalog> {
        Box::pin(async move {
            let (accounts, _) = self.read_all("/api/v1/books/accounts").await?;
            let (periods, periods_page) = self.read_all("/api/v1/books/periods").await?;
            let (mut vendors, _) = self
                .read_all("/api/v1/books/contacts?contact_type=vendor")
                .await?;
            let (both, _) = self
                .read_all("/api/v1/books/contacts?contact_type=both")
                .await?;
            vendors.extend(both);
            let (segments, segments_page) = self.read_all("/api/v1/books/segments").await?;
            Ok(Catalog {
                accounts: accounts.iter().map(account).collect::<Result<_, _>>()?,
                periods: periods.iter().map(period).collect::<Result<_, _>>()?,
                ledger_start_date: match periods_page.get("ledger_start_date") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(date(v, "ledger_start_date")?),
                },
                vendors: vendors.iter().map(vendor).collect::<Result<_, _>>()?,
                segments: segments.iter().map(segment).collect::<Result<_, _>>()?,
                segmentation_enabled: segments_page
                    .get("segmentation_enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
    }
}

fn unreadable(what: &str) -> LedgerError {
    LedgerError::new(
        Code::Timeout,
        format!("VisiBooks returned {what}; try the sync again"),
    )
}

/// An id or code as text: VisiBooks sends numbers, Payvy stores strings.
fn text(row: &Value, field: &str) -> Option<String> {
    match row.get(field)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn required(row: &Value, field: &str) -> Result<String, LedgerError> {
    text(row, field).ok_or_else(|| unreadable(&format!("a row without {field}")))
}

fn flag(row: &Value, field: &str) -> bool {
    row.get(field).and_then(Value::as_bool).unwrap_or(false)
}

fn date(value: &Value, field: &str) -> Result<NaiveDate, LedgerError> {
    value
        .as_str()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .ok_or_else(|| unreadable(&format!("an unreadable {field}")))
}

fn account(row: &Value) -> Result<CatalogAccount, LedgerError> {
    Ok(CatalogAccount {
        id: required(row, "id")?,
        number: text(row, "number"),
        name: text(row, "name").unwrap_or_default(),
        account_type: required(row, "account_type")?,
        sub_type: text(row, "sub_type"),
        active: flag(row, "active"),
        required_segment_type_ids: row
            .get("required_segment_type_ids")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(|v| match v {
                        Value::Number(n) => Some(n.to_string()),
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn period(row: &Value) -> Result<CatalogPeriod, LedgerError> {
    Ok(CatalogPeriod {
        id: required(row, "id")?,
        name: text(row, "name").unwrap_or_default(),
        start: date(row.get("start_date").unwrap_or(&Value::Null), "start_date")?,
        end: date(row.get("end_date").unwrap_or(&Value::Null), "end_date")?,
        status: required(row, "status")?,
    })
}

fn vendor(row: &Value) -> Result<CatalogVendor, LedgerError> {
    Ok(CatalogVendor {
        id: required(row, "id")?,
        name: text(row, "name").unwrap_or_default(),
        email: text(row, "email"),
        archived: flag(row, "archived"),
        ap_owner: text(row, "ap_owner"),
        default_expense_account_id: text(row, "default_expense_account_id"),
        external_source: text(row, "external_source"),
        // Compared exactly, as VisiBooks stores it: not trimmed.
        external_id: row.get("external_id").and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }),
    })
}

fn segment(row: &Value) -> Result<CatalogSegment, LedgerError> {
    let kind = row.get("segment_type").unwrap_or(&Value::Null);
    Ok(CatalogSegment {
        id: required(row, "id")?,
        name: text(row, "name").unwrap_or_default(),
        code: text(row, "code"),
        active: flag(row, "active"),
        accepts_postings: flag(row, "accepts_postings"),
        type_id: required(kind, "id")?,
        type_name: text(kind, "name").unwrap_or_default(),
    })
}
fn missing(message: &str) -> LedgerError {
    LedgerError::new(Code::MissingField, message)
}
fn id(value: &str) -> Result<i64, LedgerError> {
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
        return Err(missing("VisiBooks IDs must be positive integers"));
    }
    value
        .parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| missing("Invalid VisiBooks ID"))
}
fn key(value: &str) -> Result<(), LedgerError> {
    if value.is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":_-".contains(&b))
    {
        return Err(missing("Invalid external key"));
    }
    Ok(())
}
fn cents(value: &str) -> Result<i64, LedgerError> {
    let (whole, fraction) = value
        .split_once('.')
        .ok_or_else(|| missing("Expected a two-decimal amount"))?;
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() != 2
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(missing(
            "VisiBooks currently requires positive two-decimal line amounts",
        ));
    }
    whole
        .parse::<i64>()
        .ok()
        .and_then(|v| v.checked_mul(100))
        .and_then(|v| v.checked_add(fraction.parse::<i64>().ok()?))
        .filter(|v| *v > 0)
        .ok_or_else(|| missing("Amount is zero or exceeds the supported range"))
}
fn transport_error(_error: reqwest::Error) -> LedgerError {
    LedgerError::new(
        Code::Timeout,
        "VisiBooks could not confirm the request; retry with the same key",
    )
}
fn response_error(status: u16, code: Option<&str>) -> LedgerError {
    let (code, message) = match (status, code) {
        (401 | 403, _) => (
            Code::NeedsAuth,
            "Check VisiBooks credentials, entity access, and write permission",
        ),
        (429 | 503, _) | (_, Some("BUSY")) => (
            Code::Concurrency,
            "VisiBooks is busy; retry the same request",
        ),
        (_, Some("PERIOD_CLOSED")) => (Code::PeriodClosed, "Choose an open accounting period"),
        (_, Some("UNKNOWN_ACCOUNT")) => {
            (Code::UnknownAccount, "Choose an active VisiBooks account")
        }
        (_, Some("VENDOR_UNLINKED")) => (Code::VendorUnlinked, "Link this vendor to VisiBooks"),
        (_, Some("CURRENCY")) => (Code::Currency, "The ledger currency is not supported"),
        (_, Some("DUPLICATE_INVOICE")) => (
            Code::DuplicateInvoice,
            "This vendor invoice already exists in VisiBooks",
        ),
        (_, Some("MISSING_FIELD" | "VALIDATION_ERROR")) => (
            Code::MissingField,
            "Check required VisiBooks fields and coding",
        ),
        (500..=599, _) => (
            Code::Timeout,
            "VisiBooks did not confirm the request; retry the same key",
        ),
        _ => (
            Code::Conflict,
            "VisiBooks refused this operation; reconcile the existing record",
        ),
    };
    LedgerError::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BillLineBody;
    use axum::{Json, Router, extract::State, http::StatusCode, routing::any};
    use std::sync::{Arc, Mutex};

    fn bill() -> BillBody {
        BillBody {
            external_key: "payvy:bill:example".into(),
            post_date: None,
            service_period: None,
            vendor_external_id: "38".into(),
            subsidiary_id: "12".into(),
            invoice_number: "ALLIED-123".into(),
            invoice_date: "2026-11-12".into(),
            due_date: Some("2026-11-26".into()),
            memo: "Rental".into(),
            currency: "USD".into(),
            lines: vec![BillLineBody {
                account_id: "500".into(),
                amount: "330.01".into(),
                memo: "Container".into(),
                department_id: None,
                class_id: None,
                location_id: Some("71".into()),
            }],
        }
    }

    async fn mock(
        status: StatusCode,
        response: Value,
    ) -> (
        VisiBooks,
        Arc<Mutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = Router::new()
            .fallback(any({
                let response = response.clone();
                move |State(calls): State<Arc<Mutex<Vec<Value>>>>,
                      headers: header::HeaderMap,
                      Json(body): Json<Value>| {
                    let response = response.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer vk_test-only");
                        assert_eq!(headers["x-books-entity-id"], "12");
                        calls.lock().unwrap().push(body);
                        (status, Json(response))
                    }
                }
            }))
            .with_state(received.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            VisiBooks::build(&origin, "12", "vk_test-only", true).unwrap(),
            received,
            task,
        )
    }

    #[test]
    fn origins_ids_and_amounts_fail_closed() {
        for origin in [
            "http://example.com",
            "https://user:secret@example.com",
            "https://example.com/path",
            "https://example.com/?key=secret",
        ] {
            assert!(VisiBooks::new(origin, "12", "vk_test-only").is_err());
        }
        for amount in [
            "-1.00",
            "0.00",
            "1.001",
            "NaN",
            "92233720368547758.08",
            "1e2.00",
        ] {
            assert!(cents(amount).is_err());
        }
        assert_eq!(cents("92233720368547758.07").unwrap(), i64::MAX);
        assert_eq!(cents("330.01").unwrap(), 33001);
        assert!(key("../../other").is_err());
        assert!(id("38/path").is_err());
    }

    #[tokio::test]
    async fn bill_translation_preserves_cents_and_segment_and_refuses_lost_coding() {
        let (client, calls, task) = mock(
            StatusCode::CREATED,
            json!({"data":{"id":81,"external_id":"payvy:bill:example","status":"posted"}}),
        )
        .await;
        let posted = client.post_bill(&bill()).await.unwrap();
        assert_eq!(posted.external_id, "81");
        assert_eq!(
            calls.lock().unwrap()[0]["bill"]["lines"][0],
            json!({"account_id":500,"amount_cents":33001,"description":"Container","segment_id":71})
        );
        let mut changed = bill();
        changed.lines[0].class_id = Some("72".into());
        assert_eq!(
            client.post_bill(&changed).await.unwrap_err().code,
            Code::MissingField
        );
        changed = bill();
        changed.subsidiary_id = "13".into();
        assert!(client.post_bill(&changed).await.is_err());
        assert_eq!(calls.lock().unwrap().len(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn due_date_uses_the_invoice_date_only_when_absent() {
        let (client, calls, task) = mock(
            StatusCode::CREATED,
            json!({"data":{"id":81,"external_id":"payvy:bill:example","status":"posted"}}),
        )
        .await;
        let explicit = bill();
        client.post_bill(&explicit).await.unwrap();
        let mut undated = explicit.clone();
        undated.due_date = None;
        client.post_bill(&undated).await.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0]["bill"]["due_date"], "2026-11-26");
        assert_eq!(calls[1]["bill"]["due_date"], undated.invoice_date);
        assert!(undated.due_date.is_none());
        task.abort();
    }

    #[tokio::test]
    async fn provider_errors_are_typed_without_echoing_private_response_text() {
        for (status, code, expected) in [
            (401, "UNAUTHORIZED", Code::NeedsAuth),
            (409, "PERIOD_CLOSED", Code::PeriodClosed),
            (422, "UNKNOWN_ACCOUNT", Code::UnknownAccount),
            (409, "BODY_MISMATCH", Code::Conflict),
            (409, "BUSY", Code::Concurrency),
            (502, "oops", Code::Timeout),
        ] {
            let (client, _, task) = mock(
                StatusCode::from_u16(status).unwrap(),
                json!({"error":{"code":code,"message":"vk_secret BANK DETAILS"}}),
            )
            .await;
            let error = client.post_bill(&bill()).await.unwrap_err();
            assert_eq!(error.code, expected);
            assert!(!error.to_string().contains("secret"));
            assert!(!error.to_string().contains("BANK"));
            task.abort();
        }
    }

    #[tokio::test]
    async fn redirects_and_voided_or_wrong_record_responses_are_not_success() {
        for (status, response) in [
            (StatusCode::FOUND, json!({})),
            (
                StatusCode::OK,
                json!({"data":{"id":81,"external_id":"other","status":"posted"}}),
            ),
            (
                StatusCode::OK,
                json!({"data":{"id":81,"external_id":"payvy:bill:example","status":"void"}}),
            ),
        ] {
            let (client, _, task) = mock(status, response).await;
            assert!(client.post_bill(&bill()).await.is_err());
            task.abort();
        }
    }

    #[tokio::test]
    async fn payments_record_one_application_and_void_checks_the_returned_identity() {
        let (client, calls, task) = mock(
            StatusCode::CREATED,
            json!({"data":{"id":91,"external_id":"payvy:payment:example","status":"confirmed"}}),
        )
        .await;
        let payment = PaymentBody {
            external_key: "payvy:payment:example".into(),
            bill_external_id: "81".into(),
            vendor_external_id: "38".into(),
            subsidiary_id: "12".into(),
            account_id: "600".into(),
            tran_date: "2026-11-12".into(),
            memo: "ach".into(),
            currency: "USD".into(),
            amount: "330.01".into(),
        };
        for (source, expected) in [
            ("ach", "ach"),
            ("check", "check"),
            ("wire", "wire"),
            ("card", "card"),
            ("domestic_wire", "wire"),
            ("real_time_payment", "other"),
            ("other", "other"),
        ] {
            let mut input = payment.clone();
            input.memo = source.into();
            client.post_payment(&input).await.unwrap();
            assert_eq!(
                calls.lock().unwrap().last().unwrap()["payment"]["method"],
                expected
            );
        }
        let mut unsupported = payment.clone();
        unsupported.memo = "crypto".into();
        assert_eq!(
            client.post_payment(&unsupported).await.unwrap_err().code,
            Code::MissingField
        );
        assert_eq!(calls.lock().unwrap().len(), 7);
        assert_eq!(
            calls.lock().unwrap()[0]["payment"]["applications"],
            json!([{"bill_id":81,"amount_cents":33001}])
        );
        task.abort();
        let (client, _, task) =
            mock(StatusCode::OK, json!({"data":{"id":91,"status":"void"}})).await;
        client.void_payment("91").await.unwrap();
        assert!(client.void_payment("92").await.is_err());
        task.abort();
    }

    /// Run only against the isolated Rails test server prepared by
    /// docs/visibooks-adapter.md. The JSON file contains disposable test keys.
    #[tokio::test]
    #[ignore = "requires the isolated local VisiAPI test server"]
    async fn local_visiapi_contract() {
        let path = std::env::var("PAYVY_VISIBOOKS_TEST_FIXTURE").expect("fixture path");
        let fixture: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let get = |key: &str| fixture[key].as_str().unwrap();
        let client = VisiBooks::build(get("origin"), get("entity"), get("token"), true).unwrap();
        for method in [
            "ach",
            "check",
            "wire",
            "card",
            "domestic_wire",
            "real_time_payment",
            "other",
        ] {
            let mut body = bill();
            body.due_date = None;
            body.external_key = format!("payvy:bill:{}", uuid::Uuid::new_v4());
            body.invoice_number = body.external_key.clone();
            body.vendor_external_id = get("vendor").into();
            body.subsidiary_id = get("entity").into();
            body.lines[0].account_id = get("expense").into();
            body.lines[0].location_id = None;
            let posted = client.post_bill(&body).await.unwrap();
            assert_eq!(
                client.post_bill(&body).await.unwrap().external_id,
                posted.external_id
            );
            let mut changed = body.clone();
            changed.memo = "Changed".into();
            assert_eq!(
                client.post_bill(&changed).await.unwrap_err().code,
                Code::Conflict
            );
            let payment = PaymentBody {
                external_key: format!("payvy:payment:{}", uuid::Uuid::new_v4()),
                bill_external_id: posted.external_id.clone(),
                vendor_external_id: body.vendor_external_id.clone(),
                subsidiary_id: body.subsidiary_id.clone(),
                account_id: get("bank").into(),
                tran_date: body.invoice_date.clone(),
                memo: method.into(),
                currency: "USD".into(),
                amount: "330.01".into(),
            };
            let paid = client.post_payment(&payment).await.unwrap();
            assert_eq!(
                client.post_payment(&payment).await.unwrap().external_id,
                paid.external_id
            );
            assert_eq!(
                client
                    .void_bill(&posted.external_id)
                    .await
                    .unwrap_err()
                    .code,
                Code::Conflict
            );
            client.void_payment(&paid.external_id).await.unwrap();
            client.void_payment(&paid.external_id).await.unwrap();
            client.void_bill(&posted.external_id).await.unwrap();
            client.void_bill(&posted.external_id).await.unwrap();
        }
    }

    #[tokio::test]
    async fn service_period_and_followup_calls_hit_their_endpoints() {
        let (books, received, task) = mock(
            StatusCode::OK,
            json!({ "data": { "id": 88, "external_id": "payvy:bill:example", "status": "posted" } }),
        )
        .await;
        let mut body = bill();
        body.service_period = Some(crate::ServicePeriod {
            starts_on: "2026-11-01".into(),
            ends_on: "2026-11-30".into(),
        });
        books.post_bill(&body).await.unwrap();
        books
            .set_document_link("payvy:bill:example", "https://app.payvy.com/bills/abc")
            .await
            .unwrap();
        books
            .set_bank_reference("payvy:payment:example", "ach-123")
            .await
            .unwrap();
        task.abort();
        let calls = received.lock().unwrap();
        assert_eq!(
            calls[0]["bill"]["service_period"]["starts_on"],
            "2026-11-01"
        );
        assert_eq!(calls[0]["bill"]["service_period"]["ends_on"], "2026-11-30");
        assert_eq!(calls[1]["document_link"], "https://app.payvy.com/bills/abc");
        assert_eq!(calls[2]["bank_reference"], "ach-123");
    }

    /// Paged reads: every page is followed with `after=<last id>`, vendor
    /// and "both" contacts are both read, and the page-level fields
    /// (ledger_start_date, segmentation_enabled) come through.
    #[tokio::test]
    async fn catalog_reads_every_page_of_every_list() {
        use axum::extract::{Query, Request};
        use std::collections::HashMap;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let app = Router::new()
            .fallback(any({
                let seen = seen.clone();
                move |Query(q): Query<HashMap<String, String>>, request: Request| {
                    let seen = seen.clone();
                    async move {
                        assert_eq!(request.method(), axum::http::Method::GET);
                        assert_eq!(request.headers()["x-books-entity-id"], "12");
                        let path = request.uri().path().to_owned();
                        seen.lock().unwrap().push(request.uri().to_string());
                        let after = q.get("after").cloned();
                        let body = match (path.as_str(), q.get("contact_type").map(String::as_str), after.as_deref()) {
                            ("/api/v1/books/accounts", _, None) => json!({
                                "data": [{ "id": 1, "number": "5000", "name": "Repairs", "account_type": "expense", "sub_type": null, "active": true, "required_segment_type_ids": [2] }],
                                "page": { "has_more": true } }),
                            ("/api/v1/books/accounts", _, Some("1")) => json!({
                                "data": [{ "id": 4, "number": "1000", "name": "Operating", "account_type": "asset", "sub_type": "bank", "active": true }],
                                "page": { "has_more": false } }),
                            ("/api/v1/books/periods", _, None) => json!({
                                "data": [{ "id": 3, "name": "Jan", "start_date": "2027-01-01", "end_date": "2027-01-31", "status": "locked" }],
                                "page": { "has_more": false }, "ledger_start_date": "2026-01-01" }),
                            ("/api/v1/books/contacts", Some("vendor"), None) => json!({
                                "data": [{ "id": 38, "name": "Allied", "external_source": "quarry", "external_id": "quarry_vendor_9", "archived": false, "default_expense_account_id": 1 }],
                                "page": { "has_more": false } }),
                            ("/api/v1/books/contacts", Some("both"), None) => json!({
                                "data": [{ "id": 40, "name": "Acme", "archived": true }],
                                "page": { "has_more": false } }),
                            ("/api/v1/books/segments", _, None) => json!({
                                "data": [{ "id": 5, "name": "Phoenix", "code": "PHX", "active": true, "accepts_postings": true, "segment_type": { "id": 2, "name": "Location", "code": "LOC" } }],
                                "page": { "has_more": false }, "segmentation_enabled": true }),
                            other => panic!("unexpected read {other:?}"),
                        };
                        (StatusCode::OK, Json(body))
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let _task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let books = VisiBooks::build(&origin, "12", "vk_test-only", true).unwrap();
        let catalog = books.catalog().await.unwrap();
        assert_eq!(catalog.accounts.len(), 2);
        assert_eq!(catalog.accounts[0].required_segment_type_ids, vec!["2"]);
        assert_eq!(catalog.accounts[1].sub_type.as_deref(), Some("bank"));
        assert_eq!(catalog.periods[0].status, "locked");
        assert_eq!(
            catalog.ledger_start_date,
            NaiveDate::from_ymd_opt(2026, 1, 1)
        );
        assert_eq!(catalog.vendors.len(), 2);
        assert_eq!(
            catalog.vendors[0].external_id.as_deref(),
            Some("quarry_vendor_9")
        );
        assert_eq!(
            catalog.vendors[0].default_expense_account_id.as_deref(),
            Some("1")
        );
        assert!(catalog.vendors[1].archived);
        assert!(catalog.segmentation_enabled);
        assert_eq!(catalog.segments[0].type_id, "2");
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter()
                .any(|u| u.ends_with("/api/v1/books/accounts?limit=100&after=1")),
            "{seen:?}"
        );
    }

    /// A server that repeats a page can't make the sync loop.
    #[tokio::test]
    async fn catalog_refuses_ids_that_do_not_increase() {
        let app = Router::new().fallback(any(|| async {
            Json(json!({ "data": [{ "id": 7, "account_type": "expense" }], "page": { "has_more": true } }))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let _task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let books = VisiBooks::build(&origin, "12", "vk_test-only", true).unwrap();
        let e = books.catalog().await.unwrap_err();
        assert!(e.detail.contains("don't increase"), "{}", e.detail);
    }
}
