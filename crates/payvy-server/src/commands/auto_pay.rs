//! The ceiling on automatic payments. A workflow's `pay` step set to `auto`
//! pays a bill without a person, but never above the limit in force: the
//! lower of the company's limit (the default until a person sets one) and
//! the vendor's own. A bill over it waits for a person to release it
//! (`workflow::evaluate`), and the payment itself is checked again as it is
//! sent (`payments::submit`).
//!
//! Only a person changes a limit, and every change is an event. Lowering is
//! always safe and runs at once; raising lets bigger bills go without a
//! person, so it is confirmed: the preview lists the bills that would start
//! paying automatically.

use payvy_domain::Cents;
use payvy_domain::workflow::{
    self, AutoPayLimit, DEFAULT_AUTO_PAY_LIMIT_CENTS, MAX_AUTO_PAY_LIMIT_CENTS, dollars,
};
use payvy_store as store;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, bills, parse, text};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {}

/// The company's limit and every vendor's own.
async fn snapshot(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let limits = store::auto_pay_limits(&mut cx.tx).await?;
    let company = AutoPayLimit::effective(limits.company, None);
    let mut vendors = Vec::new();
    for (id, cents) in &limits.vendors {
        let name = store::get_vendor(&mut cx.tx, *id)
            .await?
            .map(|v| v.name)
            .unwrap_or_default();
        vendors.push(json!({
            "vendor_id": id, "vendor_name": name, "limit_cents": cents,
            // A vendor limit at or above the company's (the company's was
            // lowered after it) has no effect until the company's is raised.
            "in_force": *cents < company.cents,
        }));
    }
    vendors.sort_by(|a, b| a["vendor_name"].as_str().cmp(&b["vendor_name"].as_str()));
    Ok(json!({
        "company": {
            "limit_cents": company.cents,
            "source": company.source,
            "label": dollars(company.cents),
        },
        "default_cents": DEFAULT_AUTO_PAY_LIMIT_CENTS,
        "max_cents": MAX_AUTO_PAY_LIMIT_CENTS,
        "vendors": vendors,
    }))
}

pub async fn limits(cx: &mut Cx<'_>, _: Limits) -> Result<Value, CommandError> {
    snapshot(cx).await
}

/// `null` must be told apart from a missing field.
fn some<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<Cents>>, D::Error> {
    Option::<Cents>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Set {
    vendor_id: Option<Uuid>,
    #[serde(default, deserialize_with = "some")]
    limit_cents: Option<Option<Cents>>,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    Lower,
    Raise,
}

/// Bills that pay automatically right now.
async fn ready_ids(cx: &mut Cx<'_>) -> Result<Vec<Value>, CommandError> {
    let listed = bills::list(
        cx,
        parse(&json!({ "status": "ready_to_pay", "limit": 500 }))?,
    )
    .await?;
    Ok(listed["bills"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|b| {
            json!({ "bill_id": b["bill"]["id"], "vendor_name": b["bill"]["vendor_name"],
                    "invoice_number": b["bill"]["invoice_number"],
                    "amount_cents": b["bill"]["amount_cents"] })
        })
        .collect())
}

pub async fn set(cx: &mut Cx<'_>, input: Set, way: Way) -> Result<Value, CommandError> {
    let Some(new) = input.limit_cents else {
        return Err(CommandError::Invalid("limit_cents is required".into()));
    };
    if let Some(c) = new {
        workflow::check_auto_pay_limit(c).map_err(CommandError::Invalid)?;
    }
    if new.is_none() && (way == Way::Lower || input.vendor_id.is_none()) {
        return Err(CommandError::Invalid(
            "only a vendor's own limit can be removed (limit_cents null, with vendor_id, \
             through auto_pay.raise_limit); the company always has a limit"
                .into(),
        ));
    }
    // One change at a time, and none while an automatic payment is being sent.
    store::lock_auto_pay_limits(&mut cx.tx, cx.org).await?;
    let limits = store::auto_pay_limits(&mut cx.tx).await?;
    let company = AutoPayLimit::effective(limits.company, None).cents;
    let vendor = match input.vendor_id {
        Some(id) => Some(
            store::get_vendor(&mut cx.tx, id)
                .await?
                .ok_or(CommandError::NotFound("vendor"))?,
        ),
        None => None,
    };
    let own = input
        .vendor_id
        .and_then(|id| limits.vendors.get(&id).copied());
    // What applies now, and what will.
    let (from, to) = match &vendor {
        None => (company, new.unwrap_or(company)),
        Some(_) => (
            own.unwrap_or(company).min(company),
            new.unwrap_or(company).min(company),
        ),
    };
    let label = match &vendor {
        Some(v) => format!("{}'s auto-pay limit", v.name),
        None => "the company's auto-pay limit".to_owned(),
    };
    if let (Some(v), Some(c)) = (&vendor, new)
        && c >= company
    {
        return Err(CommandError::Invalid(format!(
            "a vendor's limit must be lower than the company's {}; to let {} pay up to that, \
             remove its own limit (limit_cents null)",
            dollars(company),
            v.name
        )));
    }
    let unchanged = |was: Cents, will: Cents| {
        let (more, use_) = match way {
            Way::Lower => ("lower", "auto_pay.raise_limit to raise"),
            Way::Raise => ("higher", "auto_pay.lower_limit to lower"),
        };
        CommandError::Conflict(format!(
            "{label} is {}; {} isn't {more}. Use {use_} it",
            dollars(was),
            dollars(will)
        ))
    };
    match (way, &vendor) {
        (Way::Lower, _) if to >= from => return Err(unchanged(from, to)),
        (Way::Raise, None) if to <= from => return Err(unchanged(from, to)),
        (Way::Raise, Some(v)) => match (own, new) {
            (None, _) => {
                return Err(CommandError::Conflict(format!(
                    "{} has no limit of its own; it follows the company's {}. Raise that instead",
                    v.name,
                    dollars(company)
                )));
            }
            (Some(was), Some(will)) if will <= was => return Err(unchanged(was, will)),
            _ => {}
        },
        _ => {}
    }

    let before = ready_ids(cx).await?;
    store::set_auto_pay_limit(
        &mut cx.tx,
        cx.org,
        input.vendor_id,
        new,
        &cx.actor.name,
        cx.now,
    )
    .await?;
    let after = ready_ids(cx).await?;
    let started: Vec<Value> = after
        .iter()
        .filter(|b| !before.iter().any(|x| x["bill_id"] == b["bill_id"]))
        .cloned()
        .collect();
    let stopped: Vec<Value> = before
        .iter()
        .filter(|b| !after.iter().any(|x| x["bill_id"] == b["bill_id"]))
        .cloned()
        .collect();

    let kind = match way {
        Way::Lower => "auto_pay.limit_lowered",
        Way::Raise => "auto_pay.limit_raised",
    };
    let reason = text(input.reason);
    cx.emit(
        kind,
        input.vendor_id.unwrap_or(cx.org),
        json!({
            "vendor_id": input.vendor_id,
            "scope": if vendor.is_some() { "vendor" } else { "company" },
            "from_cents": match &vendor { Some(_) => own, None => Some(company) },
            "to_cents": new,
            "in_force_from_cents": from,
            "in_force_to_cents": to,
            "reason": reason,
            "now_pays_automatically": started.iter().map(|b| &b["bill_id"]).collect::<Vec<_>>(),
            "now_waits_for_a_person": stopped.iter().map(|b| &b["bill_id"]).collect::<Vec<_>>(),
        }),
    )
    .await?;
    let mut out = snapshot(cx).await?;
    out["change"] = json!({
        "what": label,
        "from_cents": from,
        "to_cents": to,
        "now_pays_automatically": started,
        "now_waits_for_a_person": stopped,
    });
    Ok(out)
}
