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
            "available_cents": amounts.available_cents(),
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
    let reason = text(Some(input.reason))
        .ok_or_else(|| CommandError::Invalid("a reason is required".to_owned()))?;
    let po = must_po(cx, input.po_id).await?;
    if po.status != "draft" && po.status != "active" {
        return Err(CommandError::Conflict(format!(
            "PO {} is already {}",
            po.po_number, po.status
        )));
    }
    let amounts = po_store::amounts(&mut cx.tx, &po).await?;
    if amounts.reserved_cents > 0 {
        return Err(CommandError::Conflict(format!(
            "PO {} still has {} reserved; reverse those allocations first",
            po.po_number, amounts.reserved_cents
        )));
    }
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
        candidates.push(json!({
            "po_id": po.id, "po_number": po.po_number,
            "verified_reference": input.po_number.as_deref() == Some(po.po_number.as_str()),
            "available_cents": amounts.available_cents(),
            "proposed_cents": proposed,
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
}

fn fingerprint(bill: &store::Bill, kind: &str, allocs: &[po_store::Allocation]) -> String {
    let mut parts: Vec<String> = allocs
        .iter()
        .map(|a| format!("{}:{}:{}", a.po_id, a.amount_cents, a.state))
        .collect();
    parts.sort();
    format!(
        "bill:{}:{}:{}|{}",
        bill.id,
        bill.amount_cents,
        kind,
        parts.join(",")
    )
}

/// The bill's current PO exception, if any. Only vendors with PO or funding
/// requirements can raise one; everyone else's bills are untouched.
pub async fn bill_exception(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
) -> Result<Option<Exception>, CommandError> {
    let settings = po_store::vendor_settings(&mut cx.tx, bill.vendor_id).await?;
    let allocs = po_store::allocations_for_bill(&mut cx.tx, bill.id).await?;
    let covered: Cents = allocs.iter().map(|a| a.amount_cents).sum();
    for a in &allocs {
        if let Some(po) = po_store::get_po(&mut cx.tx, a.po_id).await?
            && (po.status != "active" || po.vendor_id != bill.vendor_id)
        {
            return Ok(Some(Exception {
                kind: "ineligible_po",
                excess_cents: a.amount_cents,
                detail: format!(
                    "PO {} is {} and no longer eligible",
                    po.po_number, po.status
                ),
            }));
        }
    }
    if settings.require_po && covered == 0 {
        return Ok(Some(Exception {
            kind: "no_po",
            excess_cents: bill.amount_cents,
            detail: "this vendor's bills require a PO and none is allocated".into(),
        }));
    }
    if settings.require_po && covered < bill.amount_cents {
        return Ok(Some(Exception {
            kind: "uncovered",
            excess_cents: bill.amount_cents - covered,
            detail: format!(
                "{} of this bill is not covered by a PO",
                bill.amount_cents - covered
            ),
        }));
    }
    if settings.require_funding
        && !po_store::bill_has_signal(&mut cx.tx, bill.id, "customer_funded").await?
    {
        return Ok(Some(Exception { kind: "funding_not_verified", excess_cents: 0,
            detail: "customer funding is required and no customer_funded signal is on record (a signal, not verified payment)".into() }));
    }
    Ok(None)
}

async fn valid_override(
    cx: &mut Cx<'_>,
    bill: &store::Bill,
    ex: &Exception,
) -> Result<bool, CommandError> {
    let allocs = po_store::allocations_for_bill(&mut cx.tx, bill.id).await?;
    let Some(o) = po_store::latest_override(&mut cx.tx, bill.id).await? else {
        return Ok(false);
    };
    Ok(o.exception_kind == ex.kind
        && o.bill_amount_cents == bill.amount_cents
        && o.authorized_excess_cents >= ex.excess_cents
        && o.fingerprint == fingerprint(bill, ex.kind, &allocs))
}

/// Refuse payment while a PO-required (or funding-required) bill has an
/// unresolved exception. Called by bill.release, bill.auto_pay and
/// bill.mark_paid before any money moves or is recorded.
pub async fn ensure_payable(cx: &mut Cx<'_>, bill: &store::Bill) -> Result<(), CommandError> {
    let Some(ex) = bill_exception(cx, bill).await? else {
        return Ok(());
    };
    if valid_override(cx, bill, &ex).await? {
        return Ok(());
    }
    Err(CommandError::Conflict(format!(
        "this bill can't be paid yet: {} ({}) — allocate it against a PO, or have a person permit it with po.override",
        ex.kind, ex.detail
    )))
}

/// What bill.get shows beside the invoice, and what the web app renders.
pub async fn bill_po_view(cx: &mut Cx<'_>, bill: &store::Bill) -> Result<Value, CommandError> {
    let allocs = po_store::allocations_for_bill(&mut cx.tx, bill.id).await?;
    let covered: Cents = allocs.iter().map(|a| a.amount_cents).sum();
    let mut rows = Vec::new();
    for a in &allocs {
        let po = po_store::get_po(&mut cx.tx, a.po_id).await?;
        rows.push(json!({ "allocation_id": a.id, "po_id": a.po_id,
            "po_number": po.as_ref().map(|p| p.po_number.clone()),
            "amount_cents": a.amount_cents, "state": a.state, "kind": a.kind }));
    }
    let ex = bill_exception(cx, bill).await?;
    let overridden = match &ex {
        Some(e) => valid_override(cx, bill, e).await?,
        None => false,
    };
    Ok(json!({
        "covered_cents": covered,
        "uncovered_cents": bill.amount_cents - covered,
        "allocations": rows,
        "exception": ex.as_ref().map(|e| json!({ "kind": e.kind, "excess_cents": e.excess_cents, "detail": e.detail })),
        "overridden": overridden,
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
    let reason = text(Some(input.reason))
        .ok_or_else(|| CommandError::Invalid("a reason is required".to_owned()))?;
    let bill = store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    let Some(ex) = bill_exception(cx, &bill).await? else {
        return Err(CommandError::Conflict(
            "this bill has no PO exception to override".into(),
        ));
    };
    if input.authorized_excess_cents < ex.excess_cents {
        return Err(CommandError::Invalid(format!(
            "authorized_excess_cents {} is below this exception's excess {}",
            input.authorized_excess_cents, ex.excess_cents
        )));
    }
    let allocs = po_store::allocations_for_bill(&mut cx.tx, bill.id).await?;
    let fp = fingerprint(&bill, ex.kind, &allocs);
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

/// Release a bill's allocations when it is rejected or voided.
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
