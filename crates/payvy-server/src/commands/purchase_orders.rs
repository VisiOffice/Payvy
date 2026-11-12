//! Purchase orders and the allocation ledger (PO Matching Spec v2, step 1:
//! lifecycle + amounts; matching UI and the payment gate are step 2).
//! Matching here is preparation only: reserving or accepting an allocation
//! never approves a bill, and only a person activates/closes/cancels a PO.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use payvy_domain::Cents;
use payvy_domain::purchase_orders::allowance_cents;
use payvy_store as store;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use store::purchase_orders as po_store;
use uuid::Uuid;

use super::{CommandError, Cx, text};

fn some<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LineIn {
    description: String,
    quantity: Option<f64>,
    unit: Option<String>,
    unit_price_cents: Option<Cents>,
    amount_cents: Cents,
    account: Option<String>,
}

fn lines(input: &[LineIn]) -> Result<Vec<po_store::NewLine>, CommandError> {
    let mut out = Vec::new();
    for l in input {
        let description = l.description.trim().to_owned();
        if description.is_empty() || l.amount_cents <= 0 {
            return Err(CommandError::Invalid(
                "each PO line needs a description and an amount over 0".into(),
            ));
        }
        out.push(po_store::NewLine {
            description,
            quantity: l.quantity,
            unit: text(l.unit.clone()),
            unit_price_cents: l.unit_price_cents,
            amount_cents: l.amount_cents,
            account: text(l.account.clone()),
        });
    }
    Ok(out)
}

async fn make_revision(
    cx: &mut Cx<'_>,
    po: &po_store::PurchaseOrder,
    authorized: Cents,
    new_lines: Option<Vec<po_store::NewLine>>,
    reason: Option<String>,
) -> Result<Uuid, CommandError> {
    if authorized <= 0 {
        return Err(CommandError::Invalid(
            "authorized_cents must be over 0".into(),
        ));
    }
    let settings = po_store::vendor_settings(&mut cx.tx, po.vendor_id).await?;
    let allowance = allowance_cents(
        authorized,
        settings.overage_cents_cap,
        settings.overage_percent_bp.map(i64::from),
    );
    let lines = match new_lines {
        Some(l) => l,
        None => match &po.revision {
            Some(r) => po_store::lines_for(&mut cx.tx, r.id)
                .await?
                .into_iter()
                .map(|l| po_store::NewLine {
                    description: l.description,
                    quantity: l.quantity,
                    unit: l.unit,
                    unit_price_cents: l.unit_price_cents,
                    amount_cents: l.amount_cents,
                    account: l.account,
                })
                .collect(),
            None => Vec::new(),
        },
    };
    let rev_no = po.revision.as_ref().map_or(1, |r| r.revision + 1);
    Ok(po_store::insert_revision(
        &mut cx.tx,
        cx.org,
        po.id,
        rev_no,
        authorized,
        settings.overage_cents_cap,
        settings.overage_percent_bp,
        allowance,
        reason.as_deref(),
        &lines,
        &cx.actor.name,
        cx.now,
    )
    .await?)
}

async fn view(cx: &mut Cx<'_>, po: &po_store::PurchaseOrder) -> Result<Value, CommandError> {
    let amounts = po_store::amounts(&mut cx.tx, po).await?;
    let lines = match &po.revision {
        Some(r) => po_store::lines_for(&mut cx.tx, r.id).await?,
        None => Vec::new(),
    };
    let allocations = po_store::allocations_for(&mut cx.tx, po.id).await?;
    Ok(json!({
        "id": po.id, "vendor_id": po.vendor_id, "vendor_name": po.vendor_name,
        "po_number": po.po_number, "status": po.status, "currency": po.currency,
        "owner": po.owner, "activated_by": po.activated_by,
        "external_source": po.external_source, "external_id": po.external_id,
        "service_address": po.service_address,
        "period_start": po.period_start, "period_end": po.period_end,
        "lifecycle_reason": po.lifecycle_reason, "created_by": po.created_by,
        "revision": po.revision.as_ref().map(|r| json!({
            "revision": r.revision, "authorized_cents": r.authorized_cents,
            "allowance_cents": r.allowance_cents, "reason": r.reason,
            "created_by": r.created_by })),
        "amounts": {
            "authorized_cents": amounts.authorized_cents,
            "reserved_cents": amounts.reserved_cents,
            "consumed_cents": amounts.consumed_cents,
            "allowance_cents": amounts.allowance_cents,
            // Nothing is available on a closed or canceled PO.
            "available_cents": amounts.available_cents_for(&po.status),
            "progress": amounts.progress(),
        },
        "lines": lines, "allocations": allocations,
    }))
}

async fn must_po(cx: &mut Cx<'_>, id: Uuid) -> Result<po_store::PurchaseOrder, CommandError> {
    po_store::get_po(&mut cx.tx, id)
        .await?
        .ok_or(CommandError::NotFound("purchase order"))
}

// ── The company-wide switch ────────────────────────────────────────────────

pub const TURNED_OFF: &str = "Purchase orders are turned off for this company. An admin can turn them on in Settings > Purchase orders.";

/// Whether this company uses purchase orders. Off until an admin turns it on.
pub async fn enabled(cx: &mut Cx<'_>) -> Result<bool, CommandError> {
    Ok(po_store::company_settings(&mut cx.tx).await?.enabled)
}

/// Every po.* write starts here; reads work either way.
async fn ensure_enabled(cx: &mut Cx<'_>) -> Result<(), CommandError> {
    if enabled(cx).await? {
        Ok(())
    } else {
        Err(CommandError::Conflict(TURNED_OFF.into()))
    }
}

fn settings_view(s: &po_store::CompanySettings) -> Value {
    json!({ "enabled": s.enabled, "updated_at": s.updated_at, "updated_by": s.updated_by })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoInput {}

pub async fn settings(cx: &mut Cx<'_>, _input: NoInput) -> Result<Value, CommandError> {
    Ok(settings_view(
        &po_store::company_settings(&mut cx.tx).await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configure {
    enabled: bool,
}

/// Turn purchase orders on or off for the company. Off never blocks a
/// payment: the gate does nothing, and allocations already made stay on
/// record (rejecting or voiding a bill still releases them).
pub async fn configure(cx: &mut Cx<'_>, input: Configure) -> Result<Value, CommandError> {
    po_store::lock_company_settings(&mut cx.tx, cx.org).await?;
    let cur = po_store::company_settings(&mut cx.tx).await?;
    if cur.enabled == input.enabled {
        return Err(CommandError::Conflict(format!(
            "purchase orders are already {}",
            if cur.enabled { "on" } else { "off" }
        )));
    }
    po_store::set_company_settings(&mut cx.tx, cx.org, input.enabled, &cx.actor.name, cx.now)
        .await?;
    cx.emit(
        "po.settings_changed",
        cx.org,
        json!({ "from": { "enabled": cur.enabled }, "to": { "enabled": input.enabled } }),
    )
    .await?;
    settings(cx, NoInput {}).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    vendor_id: Uuid,
    po_number: String,
    owner: Option<String>,
    authorized_cents: Option<Cents>,
    service_address: Option<String>,
    period_start: Option<NaiveDate>,
    period_end: Option<NaiveDate>,
    external_source: Option<String>,
    external_id: Option<String>,
    #[serde(default)]
    lines: Vec<LineIn>,
}

pub async fn create(cx: &mut Cx<'_>, input: Create) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    let number = input.po_number.trim().to_owned();
    if number.is_empty() {
        return Err(CommandError::Invalid("po_number is required".into()));
    }
    store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    if po_store::po_by_number(&mut cx.tx, &number).await?.is_some() {
        return Err(CommandError::Conflict(format!(
            "a purchase order {number} already exists"
        )));
    }
    let id = po_store::insert_po(
        &mut cx.tx,
        cx.org,
        &po_store::NewPo {
            vendor_id: input.vendor_id,
            po_number: &number,
            owner: text(input.owner).as_deref(),
            external_source: text(input.external_source.clone()).as_deref(),
            external_id: text(input.external_id).as_deref(),
            service_address: text(input.service_address).as_deref(),
            period_start: input.period_start,
            period_end: input.period_end,
            created_by: &cx.actor.name,
        },
        cx.now,
    )
    .await?;
    let po = must_po(cx, id).await?;
    if let Some(a) = input.authorized_cents {
        make_revision(cx, &po, a, Some(lines(&input.lines)?), None).await?;
    } else if !input.lines.is_empty() {
        return Err(CommandError::Invalid(
            "lines need authorized_cents (a revision) to attach to".into(),
        ));
    }
    cx.emit(
        "po.created",
        id,
        json!({ "po_number": number, "vendor_id": input.vendor_id }),
    )
    .await?;
    {
        let fresh = must_po(cx, id).await?;
        view(cx, &fresh).await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    po_id: Uuid,
    #[serde(default, deserialize_with = "some")]
    owner: Option<Option<String>>,
    authorized_cents: Option<Cents>,
    reason: Option<String>,
    #[serde(default, deserialize_with = "some")]
    service_address: Option<Option<String>>,
    #[serde(default, deserialize_with = "some")]
    period_start: Option<Option<NaiveDate>>,
    #[serde(default, deserialize_with = "some")]
    period_end: Option<Option<NaiveDate>>,
    lines: Option<Vec<LineIn>>,
}

pub async fn update(cx: &mut Cx<'_>, input: Update) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    let po = must_po(cx, input.po_id).await?;
    if po.status == "closed" || po.status == "canceled" {
        return Err(CommandError::Conflict(format!(
            "PO {} is {}; it cannot change",
            po.po_number, po.status
        )));
    }
    if input.owner.is_some()
        || input.service_address.is_some()
        || input.period_start.is_some()
        || input.period_end.is_some()
    {
        po_store::update_po_details(
            &mut cx.tx,
            po.id,
            input.owner.clone().unwrap_or(po.owner.clone()).as_deref(),
            input
                .service_address
                .clone()
                .unwrap_or(po.service_address.clone())
                .as_deref(),
            input.period_start.unwrap_or(po.period_start),
            input.period_end.unwrap_or(po.period_end),
            cx.now,
        )
        .await?;
    }
    if let Some(a) = input.authorized_cents {
        let po = must_po(cx, input.po_id).await?;
        make_revision(
            cx,
            &po,
            a,
            input.lines.as_deref().map(lines).transpose()?,
            input.reason.clone(),
        )
        .await?;
        cx.emit(
            "po.revised",
            po.id,
            json!({ "authorized_cents": a, "reason": input.reason }),
        )
        .await?;
    } else if let Some(l) = &input.lines {
        let a = po
            .revision
            .as_ref()
            .map(|r| r.authorized_cents)
            .ok_or_else(|| {
                CommandError::Invalid("set authorized_cents before adding lines".to_owned())
            })?;
        let po = must_po(cx, input.po_id).await?;
        make_revision(cx, &po, a, Some(lines(l)?), input.reason.clone()).await?;
        cx.emit("po.revised", po.id, json!({ "reason": input.reason }))
            .await?;
    }
    {
        let fresh = must_po(cx, input.po_id).await?;
        view(cx, &fresh).await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoId {
    po_id: Uuid,
}

pub async fn activate(cx: &mut Cx<'_>, input: PoId) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    let po = must_po(cx, input.po_id).await?;
    if po.status != "draft" {
        return Err(CommandError::Conflict(format!(
            "PO {} is {}; only a draft can be activated",
            po.po_number, po.status
        )));
    }
    let Some(owner) = text(po.owner.clone()) else {
        return Err(CommandError::Invalid(
            "a PO needs an owner before it can be activated".into(),
        ));
    };
    // The owner is accountable for the PO's exceptions, so it must be someone
    // who can sign in to this company now, not just a name.
    let people = store::people(&mut cx.tx, cx.org).await?;
    if !people.iter().any(|p| p.actor == owner) {
        return Err(CommandError::Invalid(format!(
            "the owner {owner:?} isn't a person in this company; set the owner to someone in Settings > People"
        )));
    }
    if po.revision.is_none() {
        return Err(CommandError::Invalid(
            "a PO needs an authorized amount (po.update with authorized_cents) before activation"
                .into(),
        ));
    }
    // Imported opening consumption must fit the budget it activates against:
    // refuse rather than activating a PO that starts over its ceiling.
    let amounts = po_store::amounts(&mut cx.tx, &po).await?;
    if amounts.committed_cents() > amounts.ceiling_cents() {
        return Err(CommandError::Conflict(format!(
            "PO {} has {} in opening consumption, over its authorized {} plus allowance {}. \
             Raise the authorized amount (po.update) or correct the import before activating",
            po.po_number,
            payvy_domain::workflow::dollars(amounts.committed_cents()),
            payvy_domain::workflow::dollars(amounts.authorized_cents),
            payvy_domain::workflow::dollars(amounts.allowance_cents)
        )));
    }
    po_store::set_status(
        &mut cx.tx,
        po.id,
        "active",
        Some(&cx.actor.name),
        None,
        cx.now,
    )
    .await?;
    cx.emit("po.activated", po.id, json!({ "po_number": po.po_number }))
        .await?;
    {
        let fresh = must_po(cx, po.id).await?;
        view(cx, &fresh).await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Close {
    po_id: Uuid,
    reason: String,
}

pub async fn close(
    cx: &mut Cx<'_>,
    input: Close,
    status: &'static str,
) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    let reason = text(Some(input.reason))
        .ok_or_else(|| CommandError::Invalid("a reason is required".to_owned()))?;
    let po = must_po(cx, input.po_id).await?;
    if po.status != "draft" && po.status != "active" && po.status != "suspended" {
        return Err(CommandError::Conflict(format!(
            "PO {} is already {}",
            po.po_number, po.status
        )));
    }
    // Q6, as proposed in review and taken in the decisions: closing stops
    // new allocations (only active POs take them); reserved and consumed
    // allocations already made stand, and nothing is released implicitly.
    // Their bills carry ineligible_po until the allocation is reversed or
    // a person permits the bill.
    po_store::set_status(&mut cx.tx, po.id, status, None, Some(&reason), cx.now).await?;
    let kind = if status == "closed" {
        "po.closed"
    } else {
        "po.canceled"
    };
    cx.emit(
        kind,
        po.id,
        json!({ "po_number": po.po_number, "reason": reason }),
    )
    .await?;
    {
        let fresh = must_po(cx, po.id).await?;
        view(cx, &fresh).await
    }
}

/// Pause an active PO: allocations already made stand, new ones are
/// refused, and its bills wait (a suspended_po exception) until a person
/// reactivates it or permits the bill with an override.
pub async fn suspend(cx: &mut Cx<'_>, input: Close) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    let reason = text(Some(input.reason))
        .ok_or_else(|| CommandError::Invalid("a reason is required".to_owned()))?;
    let po = must_po(cx, input.po_id).await?;
    if po.status != "active" {
        return Err(CommandError::Conflict(format!(
            "PO {} is {}; only an active PO can be suspended",
            po.po_number, po.status
        )));
    }
    po_store::set_status(&mut cx.tx, po.id, "suspended", None, Some(&reason), cx.now).await?;
    cx.emit(
        "po.suspended",
        po.id,
        json!({ "po_number": po.po_number, "reason": reason }),
    )
    .await?;
    {
        let fresh = must_po(cx, po.id).await?;
        view(cx, &fresh).await
    }
}

pub async fn reactivate(cx: &mut Cx<'_>, input: PoId) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    let po = must_po(cx, input.po_id).await?;
    if po.status != "suspended" {
        return Err(CommandError::Conflict(format!(
            "PO {} is {}; only a suspended PO can be reactivated",
            po.po_number, po.status
        )));
    }
    po_store::set_status(&mut cx.tx, po.id, "active", None, None, cx.now).await?;
    cx.emit(
        "po.reactivated",
        po.id,
        json!({ "po_number": po.po_number }),
    )
    .await?;
    {
        let fresh = must_po(cx, po.id).await?;
        view(cx, &fresh).await
    }
}

pub async fn get(cx: &mut Cx<'_>, input: PoId) -> Result<Value, CommandError> {
    {
        let fresh = must_po(cx, input.po_id).await?;
        view(cx, &fresh).await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    vendor_id: Option<Uuid>,
}

pub async fn list(cx: &mut Cx<'_>, input: List) -> Result<Value, CommandError> {
    let mut out = Vec::new();
    for po in po_store::list_pos(&mut cx.tx, input.vendor_id).await? {
        out.push(view(cx, &po).await?);
    }
    Ok(json!({ "purchase_orders": out }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    vendor_id: Uuid,
    po_number: String,
    external_source: String,
    external_id: String,
    owner: Option<String>,
    authorized_cents: Option<Cents>,
    opening_consumed_cents: Option<Cents>,
    source_allocation_id: Option<String>,
    service_address: Option<String>,
    period_start: Option<NaiveDate>,
    period_end: Option<NaiveDate>,
    #[serde(default)]
    lines: Vec<LineIn>,
}

/// Imports always land as drafts; recording an owner is not activation.
pub async fn import(cx: &mut Cx<'_>, input: Import) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    if let Some(po) = po_store::po_by_external(
        &mut cx.tx,
        input.external_source.trim(),
        input.external_id.trim(),
    )
    .await?
    {
        return view(cx, &po).await;
    }
    let created = create(
        cx,
        Create {
            vendor_id: input.vendor_id,
            po_number: input.po_number,
            owner: input.owner,
            authorized_cents: input.authorized_cents,
            service_address: input.service_address,
            period_start: input.period_start,
            period_end: input.period_end,
            external_source: Some(input.external_source.clone()),
            external_id: Some(input.external_id.clone()),
            lines: input.lines,
        },
    )
    .await?;
    let id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();
    if let Some(opening) = input.opening_consumed_cents
        && opening > 0
    {
        let po = must_po(cx, id).await?;
        let rev = po.revision.as_ref().ok_or_else(|| {
            CommandError::Invalid("opening consumption needs authorized_cents".to_owned())
        })?;
        po_store::insert_allocation(
            &mut cx.tx,
            cx.org,
            po.id,
            rev.id,
            None,
            opening,
            "consumed",
            "opening_consumption",
            &cx.actor.name,
            Some(input.external_source.trim()),
            Some(input.external_id.trim()),
            text(input.source_allocation_id.clone()).as_deref(),
            cx.now,
        )
        .await?;
    }
    {
        let fresh = must_po(cx, id).await?;
        view(cx, &fresh).await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorSettingsIn {
    vendor_id: Uuid,
    require_po: Option<bool>,
    require_funding: Option<bool>,
    #[serde(default, deserialize_with = "some")]
    overage_cents_cap: Option<Option<Cents>>,
    #[serde(default, deserialize_with = "some")]
    overage_percent_bp: Option<Option<i32>>,
}

pub async fn vendor_settings(
    cx: &mut Cx<'_>,
    input: VendorSettingsIn,
) -> Result<Value, CommandError> {
    store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let s = po_store::vendor_settings(&mut cx.tx, input.vendor_id).await?;
    Ok(
        json!({ "vendor_id": input.vendor_id, "require_po": s.require_po, "overage_cents_cap": s.overage_cents_cap, "overage_percent_bp": s.overage_percent_bp }),
    )
}

pub async fn set_vendor_settings(
    cx: &mut Cx<'_>,
    input: VendorSettingsIn,
) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let cur = po_store::vendor_settings(&mut cx.tx, input.vendor_id).await?;
    let s = po_store::VendorSettings {
        require_po: input.require_po.unwrap_or(cur.require_po),
        require_funding: input.require_funding.unwrap_or(cur.require_funding),
        overage_cents_cap: input.overage_cents_cap.unwrap_or(cur.overage_cents_cap),
        overage_percent_bp: input.overage_percent_bp.unwrap_or(cur.overage_percent_bp),
    };
    if s.overage_cents_cap.is_some_and(|c| c < 0)
        || s.overage_percent_bp
            .is_some_and(|b| !(0..=10_000).contains(&b))
    {
        return Err(CommandError::Invalid(
            "overage caps must be 0 or more (percent in basis points, at most 10000)".into(),
        ));
    }
    po_store::set_vendor_settings(
        &mut cx.tx,
        cx.org,
        input.vendor_id,
        &s,
        &cx.actor.name,
        cx.now,
    )
    .await?;
    cx.emit("po.vendor_settings_set", input.vendor_id, json!({ "require_po": s.require_po, "require_funding": s.require_funding, "overage_cents_cap": s.overage_cents_cap, "overage_percent_bp": s.overage_percent_bp })).await?;
    vendor_settings(
        cx,
        VendorSettingsIn {
            vendor_id: input.vendor_id,
            require_po: None,
            require_funding: None,
            overage_cents_cap: None,
            overage_percent_bp: None,
        },
    )
    .await
}

// ── The enumerated policy registry ───────────────────────────────────────

use payvy_domain::purchase_orders::{OverPeriod, Policies, QuantityMatching};

fn policy_view(vendor_id: Uuid, v: Option<&po_store::PolicyVersion>) -> Value {
    match v {
        Some(v) => json!({ "vendor_id": vendor_id, "version": v.version,
            "over_period": v.over_period, "quantity_matching": v.quantity_matching,
            "reason": v.reason,
            "updated_by": v.created_by, "updated_at": v.created_at }),
        None => json!({ "vendor_id": vendor_id, "version": 0,
            "over_period": Policies::default().over_period.as_str(),
            "quantity_matching": Policies::default().quantity_matching.as_str(),
            "reason": null, "updated_by": null, "updated_at": null }),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyIn {
    vendor_id: Uuid,
    over_period: Option<String>,
    quantity_matching: Option<String>,
    reason: Option<String>,
}

pub async fn policy_get(cx: &mut Cx<'_>, input: PolicyIn) -> Result<Value, CommandError> {
    store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let v = po_store::current_policy(&mut cx.tx, input.vendor_id).await?;
    Ok(policy_view(input.vendor_id, v.as_ref()))
}

/// A person's policy change is a new version; bills already created keep
/// the version they pinned. Asking for what is already in force is an
/// error, never a bare success.
pub async fn policy_set(cx: &mut Cx<'_>, input: PolicyIn) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let cur = po_store::current_policy(&mut cx.tx, input.vendor_id).await?;
    let cur_policies = policies_of(cur.as_ref());
    let next_over = match input.over_period.as_deref() {
        Some(s) => OverPeriod::parse(s).ok_or_else(|| {
            CommandError::Invalid("over_period is flag_only or require_full_cover".into())
        })?,
        None => cur_policies.over_period,
    };
    let next_qty = match input.quantity_matching.as_deref() {
        Some(s) => QuantityMatching::parse(s).ok_or_else(|| {
            CommandError::Invalid("quantity_matching is flag_only or enforce".into())
        })?,
        None => cur_policies.quantity_matching,
    };
    if next_over == cur_policies.over_period && next_qty == cur_policies.quantity_matching {
        return Err(CommandError::Conflict(
            "these policies are already in force for this vendor".into(),
        ));
    }
    let reason = text(input.reason);
    let v = po_store::set_policy(
        &mut cx.tx,
        cx.org,
        input.vendor_id,
        next_over.as_str(),
        next_qty.as_str(),
        reason.as_deref(),
        &cx.actor.name,
        cx.now,
    )
    .await?;
    cx.emit(
        "po.policy_changed",
        input.vendor_id,
        json!({ "vendor_id": input.vendor_id, "version": v.version,
                "from": { "over_period": cur_policies.over_period.as_str(),
                          "quantity_matching": cur_policies.quantity_matching.as_str() },
                "to": { "over_period": next_over.as_str(),
                        "quantity_matching": next_qty.as_str() }, "reason": reason }),
    )
    .await?;
    Ok(policy_view(input.vendor_id, Some(&v)))
}

fn policies_of(v: Option<&po_store::PolicyVersion>) -> Policies {
    match v {
        Some(v) => Policies {
            over_period: OverPeriod::parse(&v.over_period).unwrap_or(OverPeriod::FlagOnly),
            quantity_matching: QuantityMatching::parse(&v.quantity_matching)
                .unwrap_or(QuantityMatching::FlagOnly),
        },
        None => Policies::default(),
    }
}

/// The policies a bill is judged by: the version it pinned at creation.
/// No pin (every bill before the registry) reads the defaults — a later
/// policy change never rewrites a bill in flight.
async fn policies_for_bill(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
) -> Result<(Policies, Option<i32>), CommandError> {
    match bill.policy_version_id {
        Some(id) => {
            let v = po_store::policy_by_id(&mut cx.tx, id).await?;
            Ok((policies_of(v.as_ref()), v.map(|v| v.version)))
        }
        None => Ok((Policies::default(), None)),
    }
}

/// Allocations whose PO's service period does not contain the bill's
/// invoice date, with the PO they belong to.
async fn over_period_allocations(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
    allocs: &[po_store::Allocation],
) -> Result<Vec<(po_store::Allocation, po_store::PurchaseOrder)>, CommandError> {
    let mut out = Vec::new();
    for a in allocs {
        if let Some(po) = po_store::get_po(&mut cx.tx, a.po_id).await?
            && payvy_domain::purchase_orders::outside_period(
                po.period_start,
                po.period_end,
                bill.invoice_date,
            )
        {
            out.push((a.clone(), po));
        }
    }
    Ok(out)
}

/// Quantity matching for one bill against one PO: the findings and the
/// ordered/billed/remaining rows a person reads. Empty when the PO's lines
/// carry no quantities — those lines are amount-only in v1's terms.
async fn quantity_check(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
    po: &po_store::PurchaseOrder,
) -> Result<(Vec<payvy_domain::quantity::Finding>, Vec<Value>), CommandError> {
    use payvy_domain::invoice_evidence::format_micro;
    use payvy_domain::quantity::{self, OrderedLine};
    let Some(rev) = &po.revision else {
        return Ok((Vec::new(), Vec::new()));
    };
    let ordered: Vec<OrderedLine> = po_store::lines_for(&mut cx.tx, rev.id)
        .await?
        .into_iter()
        .filter_map(|l| {
            l.quantity.map(|q| OrderedLine {
                description: l.description,
                unit: l.unit,
                ordered_micro: (q * 1_000_000.0).round() as i64,
                unit_price_cents: l.unit_price_cents,
            })
        })
        .collect();
    if ordered.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let raw_current = store::bill_lines(&mut cx.tx, bill.id).await?;
    // "Prior" is bills that allocated to this PO before this one did —
    // a later bill never makes an earlier bill retroactively over. Each
    // bill's lines count only in the share this PO carries of it (its
    // allocations ÷ its amount), so a bill split across POs isn't
    // counted in full against each. Unallocated (a preview): every
    // allocated bill is prior, at full share for this bill.
    let stakes = po_store::allocated_bill_lines(&mut cx.tx, po.id).await?;
    let mine = stakes.iter().find(|s| s.bill_id == bill.id);
    let current: Vec<_> = raw_current
        .iter()
        .map(|l| match mine {
            Some(s) => quantity::scaled(l, s.allocated_cents, s.total_allocated_cents),
            None => l.clone(),
        })
        .collect();
    let mut prior = Vec::new();
    for s in &stakes {
        if s.bill_id == bill.id {
            continue;
        }
        if let Some(m) = mine
            && s.first_seq >= m.first_seq
        {
            continue;
        }
        prior.extend(
            s.lines
                .iter()
                .map(|l| quantity::scaled(l, s.allocated_cents, s.total_allocated_cents)),
        );
    }
    let findings = quantity::evaluate(&ordered, &prior, &current);
    let before = quantity::billed_micro(&ordered, &prior);
    let this_bill = quantity::billed_micro(&ordered, &current);
    let rows = ordered
        .iter()
        .enumerate()
        .map(|(i, o)| {
            json!({ "po_number": po.po_number, "description": o.description,
                "unit": o.unit, "ordered": format_micro(o.ordered_micro),
                "billed": format_micro(before[i] + this_bill[i]),
                "remaining": format_micro(quantity::remaining_micro(o.ordered_micro, before[i], this_bill[i])),
                "unit_price_cents": o.unit_price_cents })
        })
        .collect();
    Ok((findings, rows))
}

/// The structured identity of a blocking finding, for an override's
/// fingerprint: PO, line, unit, the billed position and the rates — never
/// the human-readable text, which may be reworded without changing what
/// was permitted.
fn scope_key(po: &po_store::PurchaseOrder, f: &payvy_domain::quantity::Finding) -> String {
    use payvy_domain::quantity::Finding;
    match f {
        Finding::QuantityOver {
            description,
            unit,
            ordered_micro,
            billed_before_micro,
            this_bill_micro,
        } => format!(
            "qty:{}:{}:{}:{}:{}",
            po.id,
            payvy_domain::bills::invoice_key(description),
            unit.as_deref().unwrap_or(""),
            ordered_micro,
            billed_before_micro + this_bill_micro
        ),
        Finding::RateMismatch {
            description,
            ordered_cents,
            billed_cents,
        } => format!(
            "rate:{}:{}:{}:{}",
            po.id,
            payvy_domain::bills::invoice_key(description),
            ordered_cents,
            billed_cents
        ),
        Finding::UnmatchedLine { .. } | Finding::AmbiguousLine { .. } => String::new(),
    }
}

fn finding_detail(f: &payvy_domain::quantity::Finding) -> String {
    use payvy_domain::invoice_evidence::format_micro;
    use payvy_domain::quantity::Finding;
    match f {
        Finding::QuantityOver {
            description,
            ordered_micro,
            billed_before_micro,
            this_bill_micro,
            ..
        } => format!(
            "{description}: {} ordered, {} billed before this bill, {} on it",
            format_micro(*ordered_micro),
            format_micro(*billed_before_micro),
            format_micro(*this_bill_micro)
        ),
        Finding::RateMismatch {
            description,
            ordered_cents,
            billed_cents,
        } => format!(
            "{description}: billed at {} against a PO rate of {}",
            payvy_domain::workflow::dollars(*billed_cents),
            payvy_domain::workflow::dollars(*ordered_cents)
        ),
        Finding::UnmatchedLine { description } => {
            format!("{description}: no PO line matches this bill line")
        }
        Finding::AmbiguousLine { description } => {
            format!("{description}: more than one PO line matches this bill line")
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    bill_id: Uuid,
    po_number: Option<String>,
}

pub async fn match_preview(cx: &mut Cx<'_>, input: Preview) -> Result<Value, CommandError> {
    let bill = store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    let existing: Cents = po_store::allocations_for_bill(&mut cx.tx, bill.id)
        .await?
        .iter()
        .map(|a| a.amount_cents)
        .sum();
    let remaining = bill.amount_cents - existing;
    let mut candidates = Vec::new();
    let mut any_available = false;
    for po in po_store::list_pos(&mut cx.tx, Some(bill.vendor_id)).await? {
        if po.status != "active" {
            continue;
        }
        let amounts = po_store::amounts(&mut cx.tx, &po).await?;
        let proposed = remaining.min(amounts.available_cents()).max(0);
        if proposed > 0 {
            any_available = true;
        }
        let (qty_findings, _) = quantity_check(cx, &bill, &po).await?;
        candidates.push(json!({
            "po_id": po.id, "po_number": po.po_number,
            "verified_reference": input.po_number.as_deref() == Some(po.po_number.as_str()),
            "available_cents": amounts.available_cents(),
            "proposed_cents": proposed,
            "over_period": payvy_domain::purchase_orders::outside_period(
                po.period_start, po.period_end, bill.invoice_date),
            "quantity_findings": qty_findings.iter()
                .map(|f| json!({ "kind": f.kind(), "detail": finding_detail(f) }))
                .collect::<Vec<_>>(),
            "exception": if proposed >= remaining { Value::Null } else { json!("over_available") },
        }));
    }
    let exception = if candidates.is_empty() {
        json!("no_po")
    } else if !any_available && remaining > 0 {
        json!("over_available")
    } else {
        Value::Null
    };
    Ok(
        json!({ "bill_id": bill.id, "bill_amount_cents": bill.amount_cents,
        "already_allocated_cents": existing, "remaining_cents": remaining,
        "candidates": candidates, "exception": exception, "reserves_nothing": true }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Apply {
    bill_id: Uuid,
    allocations: Vec<AllocIn>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllocIn {
    po_id: Uuid,
    amount_cents: Cents,
}

/// Atomic across POs: lock in sorted order, validate the whole set, then
/// reserve everything or nothing (the transaction rolls back on any error).
pub async fn match_apply(cx: &mut Cx<'_>, input: Apply) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    if input.allocations.is_empty() {
        return Err(CommandError::Invalid(
            "allocations must not be empty".into(),
        ));
    }
    let bill = store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    let mut by_po: BTreeMap<Uuid, Cents> = BTreeMap::new();
    for a in &input.allocations {
        if a.amount_cents <= 0 {
            return Err(CommandError::Invalid(
                "allocation amounts must be over 0".into(),
            ));
        }
        *by_po.entry(a.po_id).or_default() += a.amount_cents;
    }
    let ids: Vec<Uuid> = by_po.keys().copied().collect();
    let locked = po_store::lock_pos(&mut cx.tx, &ids).await?;
    if locked.len() != ids.len() {
        return Err(CommandError::NotFound("purchase order"));
    }
    let existing: Cents = po_store::allocations_for_bill(&mut cx.tx, bill.id)
        .await?
        .iter()
        .map(|a| a.amount_cents)
        .sum();
    let total: Cents = by_po.values().sum();
    if existing + total > bill.amount_cents {
        return Err(CommandError::Conflict(format!(
            "allocations total {} over the bill amount {}",
            existing + total,
            bill.amount_cents
        )));
    }
    // Validate everything before reserving anything.
    let mut amounts_by_po = BTreeMap::new();
    for po in &locked {
        if po.status != "active" {
            return Err(CommandError::Conflict(format!(
                "PO {} is {}; only active POs take allocations",
                po.po_number, po.status
            )));
        }
        if po.vendor_id != bill.vendor_id {
            return Err(CommandError::Conflict(format!(
                "PO {} belongs to a different vendor than the bill",
                po.po_number
            )));
        }
        let amounts = po_store::amounts(&mut cx.tx, po).await?;
        if !amounts.can_reserve(by_po[&po.id]) {
            return Err(CommandError::Conflict(format!(
                "PO {} has {} available; {} would exceed its authorized amount plus pinned allowance",
                po.po_number,
                amounts.available_cents(),
                by_po[&po.id]
            )));
        }
        amounts_by_po.insert(po.id, amounts);
    }
    let mut made = Vec::new();
    for po in &locked {
        let rev = po.revision.as_ref().expect("active PO has a revision");
        let id = po_store::insert_allocation(
            &mut cx.tx,
            cx.org,
            po.id,
            rev.id,
            Some(bill.id),
            by_po[&po.id],
            "reserved",
            "match",
            &cx.actor.name,
            None,
            None,
            None,
            cx.now,
        )
        .await?;
        made.push(json!({ "allocation_id": id, "po_id": po.id, "po_number": po.po_number, "amount_cents": by_po[&po.id], "state": "reserved" }));
        cx.emit(
            "po.match_applied",
            po.id,
            json!({ "bill_id": bill.id, "allocation_id": id, "amount_cents": by_po[&po.id] }),
        )
        .await?;
    }
    Ok(
        json!({ "bill_id": bill.id, "bill_amount_cents": bill.amount_cents,
        "allocated_cents": existing + total, "uncovered_cents": bill.amount_cents - existing - total,
        "allocations": made }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllocId {
    allocation_id: Uuid,
    reason: Option<String>,
}

pub async fn allocation_accept(cx: &mut Cx<'_>, input: AllocId) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    let a = po_store::get_allocation(&mut cx.tx, input.allocation_id)
        .await?
        .ok_or(CommandError::NotFound("allocation"))?;
    if a.state != "reserved" {
        return Err(CommandError::Conflict(format!(
            "allocation is {}; only a reserved one can be accepted",
            a.state
        )));
    }
    po_store::set_allocation_state(&mut cx.tx, a.id, "consumed", &cx.actor.name, None, cx.now)
        .await?;
    cx.emit(
        "po.allocation_accepted",
        a.po_id,
        json!({ "allocation_id": a.id, "bill_id": a.bill_id, "amount_cents": a.amount_cents }),
    )
    .await?;
    {
        let fresh = must_po(cx, a.po_id).await?;
        view(cx, &fresh).await
    }
}

pub async fn allocation_reverse(cx: &mut Cx<'_>, input: AllocId) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    let reason = text(input.reason)
        .ok_or_else(|| CommandError::Invalid("a reason is required".to_owned()))?;
    let a = po_store::get_allocation(&mut cx.tx, input.allocation_id)
        .await?
        .ok_or(CommandError::NotFound("allocation"))?;
    if a.state == "released" {
        return Err(CommandError::Conflict(
            "allocation is already released".into(),
        ));
    }
    po_store::set_allocation_state(
        &mut cx.tx,
        a.id,
        "released",
        &cx.actor.name,
        Some(&reason),
        cx.now,
    )
    .await?;
    cx.emit("po.allocation_released", a.po_id, json!({ "allocation_id": a.id, "bill_id": a.bill_id, "amount_cents": a.amount_cents, "reason": reason })).await?;
    {
        let fresh = must_po(cx, a.po_id).await?;
        view(cx, &fresh).await
    }
}

// ── Exceptions, overrides and the payment gate ─────────────────────────────

pub struct Exception {
    pub kind: &'static str,
    pub excess_cents: Cents,
    pub detail: String,
    /// What an override binds to beyond the bill and its allocations: for
    /// quantity exceptions, every blocking finding, so an override made
    /// against one finding never silently permits a different one. Empty
    /// for the other kinds (their fingerprints are unchanged).
    pub scope: String,
}

/// The family an override binds to: `uncovered` and `over_available` are
/// one shortfall seen from two sides of the vendor's remaining room, which
/// changes as other bills reserve and POs are added or closed — without
/// anything about this bill changing. An override permits the shortfall,
/// so it must survive the flip; other kinds bind as themselves. The
/// family's name is `uncovered`, so fingerprints written before
/// `over_available` existed keep matching.
fn override_family(kind: &str) -> &str {
    match kind {
        "uncovered" | "over_available" => "uncovered",
        other => other,
    }
}

fn fingerprint(
    bill: &store::Bill,
    kind: &str,
    allocs: &[po_store::Allocation],
    scope: &str,
) -> String {
    let mut parts: Vec<String> = allocs
        .iter()
        .map(|a| format!("{}:{}:{}", a.po_id, a.amount_cents, a.state))
        .collect();
    parts.sort();
    let base = format!(
        "bill:{}:{}:{}|{}",
        bill.id,
        bill.amount_cents,
        override_family(kind),
        parts.join(",")
    );
    // A scope names what else the override binds to (a quantity
    // exception's findings). Empty for the older kinds, whose stored
    // fingerprints are unchanged.
    if scope.is_empty() {
        base
    } else {
        format!("{base}|{scope}")
    }
}

/// The bill's first PO exception, if any (display and ordering).
pub async fn bill_exception(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
) -> Result<Option<Exception>, CommandError> {
    Ok(bill_exceptions(cx, bill).await?.into_iter().next())
}

/// Every blocking exception standing on this bill, in gate order. Only
/// vendors with PO or funding requirements can raise any; with purchase
/// orders turned off for the company nobody's bill has one. The gate
/// requires each to be permitted — an override for one kind never clears
/// another.
pub async fn bill_exceptions(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
) -> Result<Vec<Exception>, CommandError> {
    if !enabled(cx).await? {
        return Ok(Vec::new());
    }
    let settings = po_store::vendor_settings(&mut cx.tx, bill.vendor_id).await?;
    let allocs = po_store::allocations_for_bill(&mut cx.tx, bill.id).await?;
    let covered: Cents = allocs.iter().map(|a| a.amount_cents).sum();
    // A suspended or ineligible PO is the first exception, but never the
    // only one: permitting it must not skip the coverage, funding and
    // quantity gates below (an override for one kind never clears another).
    let mut suspended: Option<Exception> = None;
    let mut ineligible: Option<Exception> = None;
    for a in &allocs {
        if let Some(po) = po_store::get_po(&mut cx.tx, a.po_id).await? {
            if po.status == "suspended" {
                if suspended.is_none() {
                    suspended = Some(Exception {
                        kind: "suspended_po",
                        excess_cents: a.amount_cents,
                        scope: String::new(),
                        detail: format!(
                            "PO {} is suspended{}; its bills wait until it is reactivated",
                            po.po_number,
                            po.lifecycle_reason
                                .as_deref()
                                .map_or(String::new(), |r| format!(" ({r})"))
                        ),
                    });
                }
            } else if (po.status != "active" || po.vendor_id != bill.vendor_id)
                && ineligible.is_none()
            {
                ineligible = Some(Exception {
                    kind: "ineligible_po",
                    excess_cents: a.amount_cents,
                    scope: String::new(),
                    detail: format!(
                        "PO {} is {} and no longer eligible",
                        po.po_number, po.status
                    ),
                });
            }
        }
    }
    let mut out: Vec<Exception> = suspended.into_iter().chain(ineligible).collect();
    // Over-period, under the policy version this bill pinned: with
    // require_full_cover the out-of-period allocations do not count as
    // covering the bill. flag_only only flags (see bill_po_view's notices).
    let (policies, _) = policies_for_bill(cx, bill).await?;
    if policies.over_period == OverPeriod::RequireFullCover {
        let outside = over_period_allocations(cx, bill, &allocs).await?;
        let excluded: Cents = outside.iter().map(|(a, _)| a.amount_cents).sum();
        if excluded > 0 {
            let po = &outside[0].1;
            out.push(Exception {
                kind: "over_period",
                excess_cents: excluded,
                scope: String::new(),
                detail: format!(
                    "invoice date {} is outside PO {}'s period ({} to {}), and this vendor's over_period policy requires full cover",
                    bill.invoice_date,
                    po.po_number,
                    po.period_start.map_or("…".into(), |d| d.to_string()),
                    po.period_end.map_or("…".into(), |d| d.to_string())
                ),
            });
        }
    }
    if settings.require_po && covered == 0 {
        out.push(Exception {
            kind: "no_po",
            excess_cents: bill.amount_cents,
            scope: String::new(),
            detail: "this vendor's bills require a PO and none is allocated".into(),
        });
    }
    if settings.require_po && covered > 0 && covered < bill.amount_cents {
        // over_available when no active PO of this vendor has room for the
        // rest either; uncovered when room exists and it just isn't used.
        let mut others_available: Cents = 0;
        for po in po_store::list_pos(&mut cx.tx, Some(bill.vendor_id)).await? {
            if po.status == "active" {
                others_available += po_store::amounts(&mut cx.tx, &po).await?.available_cents();
            }
        }
        let kind = payvy_domain::purchase_orders::shortfall_kind(
            covered,
            bill.amount_cents,
            others_available,
        );
        out.push(Exception {
            kind,
            excess_cents: bill.amount_cents - covered,
            scope: String::new(),
            detail: if kind == "over_available" {
                format!(
                    "{} of this bill is not covered by a PO, and no PO has room for the rest",
                    bill.amount_cents - covered
                )
            } else {
                format!(
                    "{} of this bill is not covered by a PO",
                    bill.amount_cents - covered
                )
            },
        });
    }
    if settings.require_funding
        && !po_store::bill_has_signal(&mut cx.tx, bill.id, "customer_funded").await?
    {
        out.push(Exception { kind: "funding_not_verified", excess_cents: 0, scope: String::new(),
            detail: "customer funding is required and no customer_funded signal is on record (a signal, not verified payment)".into() });
    }
    // Quantity matching comes last, after the coverage and funding gates:
    // a quantity finding never hides a no_po, uncovered or funding
    // exception. Under an enforcing pinned policy, every blocking finding
    // is returned together, and the exception's scope (which an override
    // binds to) names them all, so permitting one finding never silently
    // permits a different one that appears later.
    if policies.quantity_matching == QuantityMatching::Enforce {
        let mut blocking: Vec<String> = Vec::new();
        let mut scoped: Vec<String> = Vec::new();
        let mut has_quantity_over = false;
        let mut seen = std::collections::HashSet::new();
        for a in &allocs {
            if !seen.insert(a.po_id) {
                continue;
            }
            if let Some(po) = po_store::get_po(&mut cx.tx, a.po_id).await? {
                let (findings, _) = quantity_check(cx, bill, &po).await?;
                for f in findings.iter().filter(|f| f.blocks()) {
                    has_quantity_over |= f.kind() == "quantity_over";
                    blocking.push(format!("{} (PO {})", finding_detail(f), po.po_number));
                    scoped.push(scope_key(&po, f));
                }
            }
        }
        if !scoped.is_empty() {
            scoped.sort();
            blocking.sort();
            out.push(Exception {
                kind: if has_quantity_over {
                    "quantity_over"
                } else {
                    "rate_mismatch"
                },
                excess_cents: 0,
                detail: blocking.join("; "),
                scope: scoped.join("|"),
            });
        }
    }
    Ok(out)
}

/// The override that currently permits this exception, if one does.
async fn valid_override(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
    ex: &Exception,
) -> Result<Option<po_store::Override>, CommandError> {
    let allocs = po_store::allocations_for_bill(&mut cx.tx, bill.id).await?;
    // Any recorded override may permit this exception: a bill can carry
    // several (one per kind), and the newest may be for another kind.
    for o in po_store::overrides_for_bill(&mut cx.tx, bill.id).await? {
        let valid = override_family(&o.exception_kind) == override_family(ex.kind)
            && o.bill_amount_cents == bill.amount_cents
            && o.authorized_excess_cents >= ex.excess_cents
            && o.fingerprint == fingerprint(bill, ex.kind, &allocs, &ex.scope);
        if valid {
            return Ok(Some(o));
        }
    }
    Ok(None)
}

/// Whether this bill is payable only because of overrides: it stands
/// under at least one exception and every one is currently permitted.
/// Such a bill is never ready_to_pay (see Facts::payable_by_override).
pub async fn payable_by_override(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
) -> Result<bool, CommandError> {
    let exceptions = bill_exceptions(cx, bill).await?;
    if exceptions.is_empty() {
        return Ok(false);
    }
    for ex in &exceptions {
        if valid_override(cx, bill, ex).await?.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Who permitted the exception this bill is payable only because of: the
/// author of the currently valid override. None when the bill has no
/// exception (or POs are off), or no override holds.
pub async fn overridden_by(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
) -> Result<Option<String>, CommandError> {
    let Some(ex) = bill_exception(cx, bill).await? else {
        return Ok(None);
    };
    Ok(valid_override(cx, bill, &ex).await?.map(|o| o.created_by))
}

/// Everyone whose override currently permits one of this bill's standing
/// exceptions. A bill can be payable through several overrides by several
/// people; every one of them is barred from releasing it.
pub async fn overriders(cx: &mut Cx<'_>, bill: &store::Bill) -> Result<Vec<String>, CommandError> {
    let mut out = Vec::new();
    for ex in bill_exceptions(cx, bill).await? {
        if let Some(o) = valid_override(cx, bill, &ex).await?
            && !out.contains(&o.created_by)
        {
            out.push(o.created_by);
        }
    }
    Ok(out)
}

/// Whether this actor is barred from releasing the bill because one of
/// its standing exceptions stands only by their override.
pub async fn release_blocked_for(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
    actor: &str,
) -> Result<bool, CommandError> {
    Ok(overriders(cx, bill)
        .await?
        .iter()
        .any(|by| payvy_domain::duties::release_refusal(Some(by), actor).is_some()))
}

/// Separation of duties for releasing: whoever permitted any standing
/// exception can't release the bill it makes payable. Called by
/// bill.release.
pub async fn ensure_not_overrider(cx: &mut Cx<'_>, bill: &store::Bill) -> Result<(), CommandError> {
    if release_blocked_for(cx, bill, &cx.actor.name).await? {
        return Err(CommandError::Forbidden(
            payvy_domain::duties::release_refusal(Some(&cx.actor.name), &cx.actor.name)
                .unwrap_or(
                    "You permitted this bill's PO exception, so someone else must release it.",
                )
                .into(),
        ));
    }
    Ok(())
}

/// Whether the caller prepared this bill: entered it, or uploaded a
/// document filed with it (the same people the approval rule names).
async fn preparer_refusal(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
) -> Result<Option<&'static str>, CommandError> {
    let uploaded_by: Vec<String> = store::bill_uploaders(&mut cx.tx, &[bill.id])
        .await?
        .into_iter()
        .map(|(_, a)| a)
        .collect();
    Ok(payvy_domain::duties::override_refusal(
        &bill.created_by,
        &uploaded_by,
        &cx.actor.name,
    ))
}

/// Refuse payment while a PO-required (or funding-required) bill has an
/// unresolved exception. Called by bill.release, bill.auto_pay and
/// bill.mark_paid before any money moves or is recorded.
pub async fn ensure_payable(cx: &mut Cx<'_>, bill: &store::Bill) -> Result<(), CommandError> {
    // Every standing exception must be permitted; an override for one
    // kind never clears another.
    for ex in bill_exceptions(cx, bill).await? {
        if valid_override(cx, bill, &ex).await?.is_none() {
            return Err(CommandError::Conflict(format!(
                "this bill can't be paid yet: {} ({}) — allocate it against a PO, or have a person permit it with po.override",
                ex.kind, ex.detail
            )));
        }
    }
    Ok(())
}

/// What bill.get shows beside the invoice, and what the web app renders.
pub async fn bill_po_view(cx: &mut Cx<'_>, bill: &store::Bill) -> Result<Value, CommandError> {
    let on = enabled(cx).await?;
    let allocs = po_store::allocations_for_bill(&mut cx.tx, bill.id).await?;
    let covered: Cents = allocs.iter().map(|a| a.amount_cents).sum();
    let mut rows = Vec::new();
    for a in &allocs {
        let po = po_store::get_po(&mut cx.tx, a.po_id).await?;
        rows.push(json!({ "allocation_id": a.id, "po_id": a.po_id,
            "po_number": po.as_ref().map(|p| p.po_number.clone()),
            "amount_cents": a.amount_cents, "state": a.state, "kind": a.kind }));
    }
    let exceptions = bill_exceptions(cx, bill).await?;
    let ex = exceptions.first();
    let holding = match ex {
        Some(e) => valid_override(cx, bill, e).await?,
        None => None,
    };
    // "Overridden" means every standing exception is permitted; while any
    // remains, the bill still waits on a person for it. Each exception
    // reports its own permitted state and who permitted it.
    let mut all_permitted = !exceptions.is_empty();
    let mut exception_details = Vec::new();
    for e in &exceptions {
        let o = valid_override(cx, bill, e).await?;
        if o.is_none() {
            all_permitted = false;
        }
        exception_details.push(json!({ "kind": e.kind, "excess_cents": e.excess_cents,
            "detail": e.detail, "overridden": o.is_some(),
            "overridden_by": o.map(|o| o.created_by) }));
    }
    let can_override = on
        && !exceptions.is_empty()
        && !all_permitted
        && cx.can("po.override")
        && preparer_refusal(cx, bill).await?.is_none();
    // flag_only policies show beside the bill without blocking it.
    let (policies, policy_version) = policies_for_bill(cx, bill).await?;
    let mut notices = Vec::new();
    let mut quantity_lines = Vec::new();
    let mut quantity_findings = Vec::new();
    {
        let mut seen = std::collections::HashSet::new();
        for a in &allocs {
            if !seen.insert(a.po_id) {
                continue;
            }
            if let Some(po) = po_store::get_po(&mut cx.tx, a.po_id).await? {
                let (findings, rows) = quantity_check(cx, bill, &po).await?;
                quantity_lines.extend(rows);
                for f in &findings {
                    quantity_findings.push(json!({ "kind": f.kind(),
                        "detail": finding_detail(f), "po_number": po.po_number }));
                    if policies.quantity_matching == QuantityMatching::FlagOnly {
                        notices.push(json!({ "kind": f.kind(), "po_number": po.po_number,
                            "detail": format!("{} (PO {})", finding_detail(f), po.po_number) }));
                    }
                }
            }
        }
    }
    if policies.over_period == OverPeriod::FlagOnly {
        for (a, po) in over_period_allocations(cx, bill, &allocs).await? {
            notices.push(json!({ "kind": "over_period", "po_number": po.po_number,
                "allocation_id": a.id, "amount_cents": a.amount_cents,
                "detail": format!("invoice date {} is outside PO {}'s period ({} to {}); flagged by this vendor's over_period policy, not blocking",
                    bill.invoice_date, po.po_number,
                    po.period_start.map_or("…".into(), |d| d.to_string()),
                    po.period_end.map_or("…".into(), |d| d.to_string())) }));
        }
    }
    Ok(json!({
        "enabled": on,
        "can_override": can_override,
        "overridden_by": holding.as_ref().map(|o| o.created_by.clone()),
        "covered_cents": covered,
        "uncovered_cents": bill.amount_cents - covered,
        "allocations": rows,
        "exception": ex.map(|e| json!({ "kind": e.kind, "excess_cents": e.excess_cents, "detail": e.detail })),
        "exceptions": exception_details,
        "overridden": all_permitted,
        "notices": notices,
        "quantity_lines": quantity_lines,
        "quantity_findings": quantity_findings,
        "policy": { "over_period": policies.over_period.as_str(),
                    "quantity_matching": policies.quantity_matching.as_str(),
                    "version": policy_version },
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverrideIn {
    bill_id: Uuid,
    authorized_excess_cents: Cents,
    reason: String,
}

pub async fn override_exception(cx: &mut Cx<'_>, input: OverrideIn) -> Result<Value, CommandError> {
    ensure_enabled(cx).await?;
    let reason = text(Some(input.reason))
        .ok_or_else(|| CommandError::Invalid("a reason is required".to_owned()))?;
    let bill = store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    if let Some(why) = preparer_refusal(cx, &bill).await? {
        return Err(CommandError::Forbidden(why.into()));
    }
    let mut target = None;
    for ex in bill_exceptions(cx, &bill).await? {
        if valid_override(cx, &bill, &ex).await?.is_none() {
            target = Some(ex);
            break;
        }
    }
    let Some(ex) = target else {
        return Err(CommandError::Conflict(
            "this bill has no PO exception left to override".into(),
        ));
    };
    if input.authorized_excess_cents < ex.excess_cents {
        return Err(CommandError::Invalid(format!(
            "authorized_excess_cents {} is below this exception's excess {}",
            input.authorized_excess_cents, ex.excess_cents
        )));
    }
    let allocs = po_store::allocations_for_bill(&mut cx.tx, bill.id).await?;
    let fp = fingerprint(&bill, ex.kind, &allocs, &ex.scope);
    let id = po_store::insert_override(
        &mut cx.tx,
        cx.org,
        bill.id,
        ex.kind,
        input.authorized_excess_cents,
        bill.amount_cents,
        &fp,
        &reason,
        &cx.actor.name,
        cx.now,
    )
    .await?;
    cx.emit(
        "po.exception_overridden",
        bill.id,
        json!({
            "bill_id": bill.id, "override_id": id, "exception": ex.kind,
            "authorized_excess_cents": input.authorized_excess_cents, "reason": reason,
        }),
    )
    .await?;
    bill_po_view(cx, &bill).await
}

/// Release a bill's allocations when it is rejected or voided. Runs whether
/// or not purchase orders are on, so turning them off never strands capacity.
pub async fn release_bill_allocations(
    cx: &mut Cx<'_>,
    bill_id: Uuid,
    reason: &str,
) -> Result<(), CommandError> {
    let released =
        po_store::reverse_allocations_for_bill(&mut cx.tx, bill_id, &cx.actor.name, reason, cx.now)
            .await?;
    for a in released {
        cx.emit("po.allocation_released", a.po_id, json!({
            "allocation_id": a.id, "bill_id": bill_id, "amount_cents": a.amount_cents, "reason": reason,
        })).await?;
    }
    Ok(())
}
