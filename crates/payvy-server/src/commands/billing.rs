//! Settings > Billing (hosted only). The company's Admin asks for a Checkout
//! or portal link; the management app makes it from this company's id, taken
//! from the session, never from the browser. Nothing here changes what the
//! company may do: only the management app's `entitlement.set`, after Stripe
//! confirms, does (docs/hosted.md).

use serde::Deserialize;
use serde_json::{Value, json};

use super::{CommandError, Cx};
use crate::billing::{Billing, BillingError, Company, PlanId};
use payvy_store as store;

const NOT_HERE: &str = "billing isn't available here: self-hosted Payvy has no plans, and a hosted install needs PAYVY_BILLING_URL and PAYVY_BILLING_KEY";

fn billing<'a>(cx: &Cx<'a>) -> Result<&'a Billing, CommandError> {
    match (cx.hosted, cx.billing) {
        (true, Some(b)) => Ok(b),
        _ => Err(CommandError::NotConfigured(NOT_HERE)),
    }
}

impl From<BillingError> for CommandError {
    fn from(e: BillingError) -> Self {
        CommandError::Billing(e.to_string())
    }
}

pub async fn get(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let b = billing(cx)?;
    let stored = store::entitlement(&mut cx.tx, cx.org).await?;
    let plan = crate::entitlements::view(true, stored.as_ref());
    // The plan in force is known here; if the management app can't be
    // reached, show that rather than nothing.
    match b.status(cx.org).await {
        Ok(s) => Ok(json!({
            "plan": plan, "available": true,
            "subscription": s.subscription, "offers": s.offers,
        })),
        Err(e) => {
            tracing::warn!("billing status unavailable: {e}");
            Ok(json!({ "plan": plan, "available": false, "subscription": null, "offers": [] }))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkout {
    plan: PlanId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Portal {}

pub async fn checkout(cx: &mut Cx<'_>, input: Checkout) -> Result<Value, CommandError> {
    open(cx, Some(input.plan)).await
}

pub async fn portal(cx: &mut Cx<'_>, _: Portal) -> Result<Value, CommandError> {
    open(cx, None).await
}

/// The link, and an event saying who opened it. Preview asks nothing.
async fn open(cx: &mut Cx<'_>, plan: Option<PlanId>) -> Result<Value, CommandError> {
    let b = billing(cx)?;
    let auth = cx.auth().ok_or(CommandError::NotConfigured(
        "billing needs PAYVY_PUBLIC_URL",
    ))?;
    let org = store::organization(&mut cx.tx, cx.org)
        .await?
        .ok_or(CommandError::NotFound("organization"))?;
    let person = store::people(&mut cx.tx, cx.org)
        .await?
        .into_iter()
        .find(|p| p.actor == cx.actor.name)
        .ok_or_else(|| CommandError::Forbidden("billing is for a signed-in person".into()))?;
    let return_url = format!("{}/settings/billing", auth.public_url);
    let company = Company {
        org_id: cx.org,
        name: &org.name,
        email: &person.email,
        return_url: &return_url,
    };

    let kind = if plan.is_some() { "checkout" } else { "portal" };
    let (kind, url) = if cx.preview {
        (kind.to_owned(), None)
    } else {
        let link = match plan {
            Some(p) => b.checkout(&company, p).await?,
            None => b.portal(&company).await?,
        };
        // Only a Stripe page over https goes to the browser.
        if !matches!(link.kind.as_str(), "checkout" | "portal") || !link.url.starts_with("https://")
        {
            return Err(CommandError::Billing(
                "billing answered without a usable link; try again".into(),
            ));
        }
        (link.kind, Some(link.url))
    };
    // The link itself is a bearer URL for the company's billing: it goes to
    // the person who asked, never into the event.
    cx.emit(
        if kind == "portal" {
            "billing.portal_opened"
        } else {
            "billing.checkout_opened"
        },
        cx.org,
        json!({ "plan": plan.map(PlanId::name) }),
    )
    .await?;
    Ok(json!({ "kind": kind, "url": url }))
}
