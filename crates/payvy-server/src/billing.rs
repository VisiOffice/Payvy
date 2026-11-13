//! The hosted management app's billing API, asked server to server by the
//! `billing.*` commands (docs/hosted.md). Payvy keeps no prices, plans or
//! Stripe state: the management app owns them, and only its
//! `entitlement.set` changes what a company may do. A Checkout or portal link
//! grants nothing by itself.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// The plans the core knows by name, because each grants a different
/// entitlement. Their names, prices and terms come from the management app.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanId {
    Byoa,
    Managed,
}

impl PlanId {
    pub fn name(self) -> &'static str {
        match self {
            PlanId::Byoa => "byoa",
            PlanId::Managed => "managed",
        }
    }
}

/// What Settings > Billing shows. Only these fields pass through, whatever
/// else the management app sends, and nothing here is a link.
#[derive(Debug, Deserialize, Serialize)]
pub struct Status {
    pub subscription: Option<Subscription>,
    #[serde(default)]
    pub offers: Vec<Offer>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Subscription {
    pub plan: Option<PlanId>,
    pub status: String,
    pub trial_end: Option<DateTime<Utc>>,
    pub current_period_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub cancel_at_period_end: bool,
    /// When payments stop if a failed payment isn't fixed, from the
    /// management app's grace policy.
    pub payments_stop_at: Option<DateTime<Utc>>,
}

/// A plan as the management app offers it: the price and trial are its to
/// state, so the open-source app never carries them.
#[derive(Debug, Deserialize, Serialize)]
pub struct Offer {
    pub id: PlanId,
    pub name: String,
    pub price: String,
    #[serde(default)]
    pub description: Vec<String>,
    #[serde(default)]
    pub trial_days: u32,
}

/// A Checkout or portal link for the person who asked.
#[derive(Debug, Deserialize)]
pub struct Link {
    pub kind: String,
    pub url: String,
}

impl Status {
    /// Bounded text, so a misbehaving management app can't fill the page.
    fn tidy(mut self) -> Self {
        fn clip(s: &mut String, n: usize) {
            if let Some((i, _)) = s.char_indices().nth(n) {
                s.truncate(i);
            }
        }
        if let Some(sub) = &mut self.subscription {
            clip(&mut sub.status, 40);
        }
        self.offers.truncate(4);
        for o in &mut self.offers {
            clip(&mut o.name, 80);
            clip(&mut o.price, 80);
            o.description.truncate(6);
            for d in &mut o.description {
                clip(d, 200);
            }
        }
        self
    }
}

pub struct Billing {
    /// The management app, e.g. https://signup.payvy.com (PAYVY_BILLING_URL).
    url: String,
    /// Its key for these calls (PAYVY_BILLING_KEY); never sent anywhere else.
    key: String,
    http: reqwest::Client,
}

/// What the Checkout and portal calls send: the company comes from the
/// signed-in session, never from the browser.
pub struct Company<'a> {
    pub org_id: Uuid,
    pub name: &'a str,
    pub email: &'a str,
    pub return_url: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum BillingError {
    /// The management app refused (for example, an unknown plan).
    #[error("{0}")]
    Refused(String),
    /// It couldn't be reached or failed; trying again later is safe.
    #[error("billing is unavailable right now; try again in a minute")]
    Unavailable,
}

impl Billing {
    pub fn new(url: &str, key: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            url.starts_with("https://") || url.starts_with("http://127.0.0.1"),
            "PAYVY_BILLING_URL must be https (or http://127.0.0.1 in development)"
        );
        anyhow::ensure!(
            key.len() >= 32,
            "PAYVY_BILLING_KEY must be at least 32 characters"
        );
        Ok(Self {
            url: url.trim_end_matches('/').to_owned(),
            key: key.to_owned(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    pub async fn status(&self, org_id: Uuid) -> Result<Status, BillingError> {
        let req = self
            .http
            .get(format!("{}/api/billing/status", self.url))
            .query(&[("org_id", org_id.to_string())]);
        Ok(Self::parse::<Status>(self.send(req).await?)?.tidy())
    }

    pub async fn checkout(
        &self,
        company: &Company<'_>,
        plan: PlanId,
    ) -> Result<Link, BillingError> {
        let mut body = Self::body(company);
        body["plan"] = json!(plan);
        let req = self
            .http
            .post(format!("{}/api/billing/checkout", self.url))
            .json(&body);
        Self::parse(self.send(req).await?)
    }

    pub async fn portal(&self, company: &Company<'_>) -> Result<Link, BillingError> {
        let req = self
            .http
            .post(format!("{}/api/billing/portal", self.url))
            .json(&Self::body(company));
        Self::parse(self.send(req).await?)
    }

    fn parse<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, BillingError> {
        serde_json::from_value(body).map_err(|e| {
            tracing::warn!("billing answered in a shape Payvy doesn't know: {e}");
            BillingError::Unavailable
        })
    }

    fn body(c: &Company<'_>) -> Value {
        json!({
            "org_id": c.org_id,
            "company_name": c.name,
            "email": c.email,
            "return_url": c.return_url,
        })
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value, BillingError> {
        let res = req.bearer_auth(&self.key).send().await.map_err(|e| {
            tracing::warn!("billing: {}", e.without_url());
            BillingError::Unavailable
        })?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(body);
        }
        // 4xx other than auth: the management app said no, with a reason
        // meant for people. Anything else is ours to fix, not theirs.
        if status.is_client_error() && status.as_u16() != 401 && status.as_u16() != 403 {
            let message = body["error"]["message"]
                .as_str()
                .unwrap_or("billing refused the request")
                .chars()
                .take(300)
                .collect();
            return Err(BillingError::Refused(message));
        }
        tracing::warn!(%status, "billing answered with an error");
        Err(BillingError::Unavailable)
    }
}
