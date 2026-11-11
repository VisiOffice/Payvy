//! Ramp discovery and OAuth foundation. Deliberately no payment-write API.
//!
//! Schema: https://docs.ramp.com/openapi/developer-api.json (2026-11-10).
//! The dedicated Payments API is beta. Keep its observations separate from
//! Mercury's states: an unfamiliar state must not authorize another payment.

use std::{collections::HashSet, time::Duration};

use reqwest::{Client, Response, Url};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use uuid::Uuid;

const MAX_PAGES: usize = 100;
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Discovery never needs payment-write permission.
pub const DISCOVERY_SCOPES: &str =
    "business:read entities:read bank_accounts:read vendors:read payments:read";

#[derive(Clone, Copy, Debug)]
pub enum Environment {
    Production,
    Sandbox,
}

impl Environment {
    fn origin(self) -> &'static str {
        match self {
            Self::Production => "https://api.ramp.com",
            Self::Sandbox => "https://demo-api.ramp.com",
        }
    }
}

/// No raw provider bodies, tokens, URLs or transport errors reach callers.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("Ramp rejected the credentials or required scopes")]
    Unauthorized,
    #[error("Ramp rate limited the request")]
    RateLimited { retry_after_seconds: Option<u64> },
    #[error("Ramp rejected the request (HTTP {status})")]
    Rejected { status: u16 },
    #[error("Ramp could not be reached or returned a server error")]
    Unavailable,
    #[error("Ramp returned an invalid or incomplete response")]
    InvalidResponse,
    #[error("invalid Ramp request parameters")]
    InvalidInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct LastFour(String);

impl<'de> Deserialize<'de> for LastFour {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() != 4 || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(serde::de::Error::custom("expected four digits"));
        }
        Ok(Self(value))
    }
}

impl LastFour {
    pub fn masked(&self) -> String {
        format!("…{}", self.0)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Business {
    pub id: Uuid,
    pub business_name_legal: String,
    pub active: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Entity {
    pub id: Uuid,
    pub entity_name: String,
    pub currency: String,
    pub payment_accounts: Vec<EntityAccount>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EntityAccount {
    pub id: Uuid,
    pub account_name: String,
    pub account_number_last_four: LastFour,
    pub currency: String,
}

/// Ramp's account response has no available balance. Do not invent zero.
#[derive(Debug, Deserialize, Serialize)]
pub struct FundingAccount {
    pub id: Uuid,
    pub account_name: String,
    pub account_number_last_four: LastFour,
    pub financial_institution_name: Option<String>,
    pub has_ach_details: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Vendor {
    pub id: Uuid,
    pub name: String,
    pub is_active: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Fee {
    pub amount: i64,
    pub currency_code: String,
    pub minor_unit_conversion_rate: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AchInstrument {
    pub id: Uuid,
    pub account_number_last_four: LastFour,
    pub fee: Fee,
}

/// Explicit IDs prevent a vendor's default account silently changing a payee.
#[derive(Debug, Deserialize, Serialize)]
pub struct PaymentInstrument {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Amount {
    pub value: i64,
    pub currency: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Payment {
    pub id: Uuid,
    pub entity_id: Uuid,
    pub source_bank_account_id: Uuid,
    pub vendor_id: Uuid,
    pub payment_method: PaymentInstrument,
    pub amount: Amount,
    pub external_ref: Option<String>,
    pub scheduled_date: String,
    pub status: String,
    pub status_detail: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Assessment {
    Pending,
    Completed,
    Cancelled,
    /// Includes ERROR, unrecognized enums and inconsistent status/detail pairs.
    /// This is not a failure that permits repayment.
    NeedsReview,
}

impl Payment {
    /// Informational only; the server must also match the persisted attempt's
    /// source, entity, vendor, instrument, amount and external reference.
    pub fn assessment(&self) -> Assessment {
        if self.amount.value <= 0
            || self.amount.currency != "USD"
            || self.payment_method.kind != "ACH"
        {
            return Assessment::NeedsReview;
        }
        match (self.status.as_str(), self.status_detail.as_deref()) {
            ("SCHEDULED", Some("SCHEDULED"))
            | ("PROCESSING", Some("INITIATED" | "DEBITING" | "TRANSFERRING_TO_VENDOR")) => {
                Assessment::Pending
            }
            ("COMPLETED", Some("DELIVERED")) => Assessment::Completed,
            ("CANCELED", Some("CANCELED")) => Assessment::Cancelled,
            _ => Assessment::NeedsReview,
        }
    }
}

#[derive(Deserialize)]
struct Page<T> {
    data: Vec<T>,
    page: NextPage,
}

#[derive(Deserialize)]
struct NextPage {
    // Require this field, including an explicit null on the final page.
    next: serde_json::Value,
}

// No Debug: the client owns credentials.
pub struct Ramp {
    origin: Url,
    token: String,
    http: Client,
}

fn loopback(origin: &str) -> Result<Url, Error> {
    let url = Url::parse(origin).map_err(|_| Error::InvalidInput)?;
    if !cfg!(debug_assertions)
        || url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Error::InvalidInput);
    }
    Ok(url)
}

fn http_client() -> Result<Client, Error> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::Unavailable)
}

fn secret_present(s: &str) -> bool {
    !s.trim().is_empty() && !s.contains(['\r', '\n'])
}

impl Ramp {
    pub fn new(environment: Environment, token: &str) -> Result<Self, Error> {
        Self::at(Url::parse(environment.origin()).unwrap(), token)
    }

    /// Local fake only; release builds refuse overrides.
    pub fn sandbox_at(origin: &str, token: &str) -> Result<Self, Error> {
        Self::at(loopback(origin)?, token)
    }

    // Non-Ramp origins are confined to debug loopback fakes.
    fn at(origin: Url, token: &str) -> Result<Self, Error> {
        if !secret_present(token) {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            origin,
            token: token.to_owned(),
            http: http_client()?,
        })
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        self.origin.join(path).map_err(|_| Error::InvalidInput)
    }

    async fn get<T: DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        decode(response).await
    }

    async fn list<T: DeserializeOwned>(
        &self,
        path: &str,
        filters: &[(&str, &str)],
    ) -> Result<Vec<T>, Error> {
        let mut url = self.url(path)?;
        url.query_pairs_mut().extend_pairs(filters.iter().copied());
        url.query_pairs_mut().append_pair("page_size", "100");
        let mut seen = HashSet::new();
        let mut output = Vec::new();
        for _ in 0..MAX_PAGES {
            if !seen.insert(url.to_string()) {
                return Err(Error::InvalidResponse);
            }
            let page: Page<T> = self.get(url).await?;
            output.extend(page.data);
            let next = match page.page.next {
                serde_json::Value::Null => return Ok(output),
                serde_json::Value::String(next) if !next.is_empty() => next,
                _ => return Err(Error::InvalidResponse),
            };
            let candidate = self
                .origin
                .join(&next)
                .map_err(|_| Error::InvalidResponse)?;
            if candidate.origin() != self.origin.origin()
                || candidate.path() != path
                || !candidate.username().is_empty()
                || candidate.password().is_some()
                || candidate.fragment().is_some()
            {
                return Err(Error::InvalidResponse);
            }
            // Never let pagination drop the exact recovery filters.
            for (key, value) in filters {
                let values: Vec<_> = candidate
                    .query_pairs()
                    .filter(|(k, _)| k == key)
                    .map(|(_, v)| v.into_owned())
                    .collect();
                if values != [*value] {
                    return Err(Error::InvalidResponse);
                }
            }
            url = candidate;
        }
        // A partial list must not look like proof that a payment doesn't exist.
        Err(Error::InvalidResponse)
    }

    pub async fn business(&self) -> Result<Business, Error> {
        self.get(self.url("/developer/v1/business")?).await
    }

    pub async fn entities(&self) -> Result<Vec<Entity>, Error> {
        self.list("/developer/v1/entities", &[]).await
    }

    pub async fn accounts(&self) -> Result<Vec<FundingAccount>, Error> {
        self.list("/developer/v1/bank-accounts", &[]).await
    }

    /// Fetch one vendor for account selection and acceptance; never download
    /// the complete vendor directory to validate a specific destination.
    pub async fn vendor(&self, id: Uuid) -> Result<Vendor, Error> {
        let vendor: Vendor = self
            .get(self.url(&format!("/developer/v1/vendors/{id}"))?)
            .await?;
        if vendor.id != id {
            return Err(Error::InvalidResponse);
        }
        Ok(vendor)
    }

    pub async fn search_vendors(&self, name: &str) -> Result<Vec<Vendor>, Error> {
        self.list(
            "/developer/v1/vendors",
            &[("name", name), ("is_active", "true")],
        )
        .await
    }

    pub async fn vendors(&self) -> Result<Vec<Vendor>, Error> {
        self.list("/developer/v1/vendors", &[]).await
    }

    pub async fn ach_instruments(&self, vendor: Uuid) -> Result<Vec<AchInstrument>, Error> {
        let rows: Vec<serde_json::Value> = self
            .list(
                &format!("/developer/v1/vendors/{vendor}/payment-methods"),
                &[],
            )
            .await?;
        let mut instruments = Vec::new();
        for row in rows {
            let kind = row["type"].as_str().ok_or(Error::InvalidResponse)?;
            if kind != "ACH" {
                continue;
            }
            let instrument: AchInstrument =
                serde_json::from_value(row).map_err(|_| Error::InvalidResponse)?;
            if instrument.fee.amount < 0
                || instrument.fee.currency_code != "USD"
                || instrument.fee.minor_unit_conversion_rate != Some(100)
            {
                return Err(Error::InvalidResponse);
            }
            instruments.push(instrument);
        }
        Ok(instruments)
    }

    pub async fn payment(&self, id: Uuid) -> Result<Payment, Error> {
        let payment: Payment = self
            .get(self.url(&format!("/developer/v1/payments/{id}"))?)
            .await?;
        if payment.id != id {
            return Err(Error::InvalidResponse);
        }
        Ok(payment)
    }

    /// Includes cancelled payments; absence is never proof a POST wasn't sent.
    pub async fn payments_by_reference(&self, reference: &str) -> Result<Vec<Payment>, Error> {
        if reference.trim().is_empty() || reference.len() > 255 {
            return Err(Error::InvalidInput);
        }
        let payments: Vec<Payment> = self
            .list(
                "/developer/v1/payments",
                &[("external_ref", reference), ("include_canceled", "true")],
            )
            .await?;
        if payments
            .iter()
            .any(|p| p.external_ref.as_deref() != Some(reference))
        {
            return Err(Error::InvalidResponse);
        }
        Ok(payments)
    }
}

async fn decode<T: DeserializeOwned>(mut response: Response) -> Result<T, Error> {
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(match status {
            401 | 403 => Error::Unauthorized,
            429 => Error::RateLimited {
                retry_after_seconds: response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|h| h.parse().ok()),
            },
            500..=599 => Error::Unavailable,
            _ => Error::Rejected { status },
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(Error::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::InvalidResponse)
}

/// Intentionally neither Debug nor Serialize: seal these values before storage.
#[derive(Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
    pub refresh_token_expires_in: Option<u64>,
    pub scope: String,
    pub token_type: String,
}

/// OAuth transport only. Browser state, authorization, sealing and activation
/// must be implemented in the server's command layer before exposing a UI.
pub struct OAuth {
    origin: Url,
    client_id: String,
    client_secret: String,
    http: Client,
}

impl OAuth {
    pub fn new(
        environment: Environment,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Self, Error> {
        if !secret_present(client_id) || !secret_present(client_secret) {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            origin: Url::parse(environment.origin()).unwrap(),
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            http: http_client()?,
        })
    }

    pub fn sandbox_at(origin: &str, client_id: &str, client_secret: &str) -> Result<Self, Error> {
        let mut client = Self::new(Environment::Sandbox, client_id, client_secret)?;
        client.origin = loopback(origin)?;
        Ok(client)
    }

    pub fn authorize_url(
        &self,
        environment: Environment,
        redirect_uri: &str,
        state: &str,
    ) -> Result<String, Error> {
        let redirect = Url::parse(redirect_uri).map_err(|_| Error::InvalidInput)?;
        if (redirect.scheme() != "https"
            && !(redirect.scheme() == "http"
                && matches!(redirect.host_str(), Some("localhost" | "127.0.0.1"))))
            || !redirect.username().is_empty()
            || redirect.password().is_some()
            || redirect.fragment().is_some()
            || redirect.query().is_some()
            || !secret_present(state)
        {
            return Err(Error::InvalidInput);
        }
        let mut url = Url::parse(match environment {
            Environment::Production => "https://app.ramp.com/v1/authorize",
            Environment::Sandbox => "https://demo.ramp.com/v1/authorize",
        })
        .unwrap();
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", self.client_id.as_str()),
            ("scope", DISCOVERY_SCOPES),
            ("redirect_uri", redirect_uri),
            ("state", state),
        ]);
        Ok(url.into())
    }

    pub async fn authorization_code(
        &self,
        code: &str,
        redirect_uri: &str,
    ) -> Result<Tokens, Error> {
        if !secret_present(code) {
            return Err(Error::InvalidInput);
        }
        self.exchange(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ])
        .await
    }

    async fn exchange(&self, fields: &[(&str, &str)]) -> Result<Tokens, Error> {
        let response = self
            .http
            .post(self.origin.join("/developer/v1/token").unwrap())
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(fields)
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        let tokens: Tokens = decode(response).await?;
        if !secret_present(&tokens.access_token)
            || tokens.expires_in == 0
            || !tokens.token_type.eq_ignore_ascii_case("Bearer")
            || tokens
                .refresh_token
                .as_deref()
                .is_some_and(|t| !secret_present(t))
        {
            return Err(Error::InvalidResponse);
        }
        Ok(tokens)
    }

    pub async fn client_credentials(&self) -> Result<Tokens, Error> {
        self.exchange(&[
            ("grant_type", "client_credentials"),
            ("scope", DISCOVERY_SCOPES),
        ])
        .await
    }

    /// Ramp's third-party refresh flow omits the existing refresh token.
    /// Its expiry is absolute from the original grant: the caller must retain
    /// that deadline, never restart it from this response's access-token TTL.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, Error> {
        if !secret_present(refresh_token) {
            return Err(Error::InvalidInput);
        }
        let mut tokens = self
            .exchange(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .await?;
        if tokens.refresh_token.is_none() {
            tokens.refresh_token = Some(refresh_token.to_owned());
        }
        Ok(tokens)
    }
}

#[cfg(test)]
#[path = "ramp_tests.rs"]
mod tests;
