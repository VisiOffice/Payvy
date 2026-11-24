//! Cards on file. A vendor that keeps one of the company's cards charges it
//! itself, so Payvy never sends these vendors a payment: a release means
//! "approved for the vendor to charge". Payvy reads the card's charges from
//! the bank, pairs a charge with a bill only when the answer is plain
//! (`payvy_domain::cards::plain_matches`), and marks the bill paid once it
//! is cleared — with the bank's transaction as the reference, never a
//! person's say-so. Everything else waits for a person. Payvy keeps the
//! bank's card id and the last four, never a card number. See docs/cards.md.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use payvy_domain::cards::{ChargeStatus, OpenBill, OpenCharge, plain_matches};
use payvy_pay::{BankError, Card};
use payvy_store::cards::{self as cards, NewVendorCard, SeenCharge, VendorCard};
use payvy_store::{self as store, BankConnection};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, bank, text};

/// The actor recorded when Payvy pairs a charge with a bill on its own.
pub(super) const MATCHER: &str = "bank";

/// How far back a newly linked card's charges count, unless the person
/// linking it says otherwise.
const DEFAULT_LOOKBACK_DAYS: i64 = 30;

async fn mercury_connection(cx: &mut Cx<'_>) -> Result<BankConnection, CommandError> {
    let c = bank::live(cx).await?;
    if c.provider != "mercury" {
        return Err(CommandError::Conflict(
            "cards on file need a Mercury connection".into(),
        ));
    }
    Ok(c)
}

fn card_error(e: BankError) -> CommandError {
    match e {
        BankError::Unauthorized(m) => CommandError::Bank(format!(
            "Mercury won't show this token the company's cards ({m}). The token needs permission \
             to read cards; Mercury can't add it to an existing token, so make a new one and \
             reconnect with bank.connect"
        )),
        e => e.into(),
    }
}

async fn fetch_cards(cx: &Cx<'_>, c: &BankConnection) -> Result<Vec<Card>, CommandError> {
    bank::mercury(cx, c)?.cards().await.map_err(card_error)
}

/// What a vendor's card looks like to people and in events.
pub(super) fn shown(k: &VendorCard) -> Value {
    json!({ "id": k.id, "last_four": k.last_four, "nickname": k.nickname, "kind": k.kind,
            "charges_from": k.charges_from, "linked_by": k.linked_by, "linked_at": k.linked_at })
}

/// The vendor's card on file at the live paying connection, if it has one.
/// A card linked under a replaced connection doesn't count.
pub(super) async fn on_file(
    cx: &mut Cx<'_>,
    vendor_id: Uuid,
) -> Result<Option<VendorCard>, CommandError> {
    let Some(c) = store::live_paying_connection(&mut cx.tx).await? else {
        return Ok(None);
    };
    Ok(cards::active_vendor_card(&mut cx.tx, vendor_id)
        .await?
        .filter(|k| k.connection_id == c.id))
}

/// What a release of a card vendor's bill does, for its preview and event.
pub(super) fn plan(k: &VendorCard, vendor: &str) -> Value {
    json!({ "card": format!("••{}", k.last_four), "nickname": k.nickname,
            "then": format!("{vendor} charges the card on file; Payvy marks the bill paid when the charge posts") })
}

// ── Reading ─────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ListCards {}

/// The company's cards at the connected bank, and which vendor each is on
/// file with.
pub async fn list_cards(cx: &mut Cx<'_>, _input: ListCards) -> Result<Value, CommandError> {
    let c = mercury_connection(cx).await?;
    let found = fetch_cards(cx, &c).await?;
    let linked = cards::vendor_cards_at(&mut cx.tx, c.id).await?;
    let mut out = Vec::new();
    for card in found {
        let mut v = json!(card);
        if let Some(k) = linked.iter().find(|k| k.card_id == card.id) {
            let vendor = store::get_vendor(&mut cx.tx, k.vendor_id).await?;
            v["on_file_with"] = json!({ "vendor_id": k.vendor_id,
                                        "vendor_name": vendor.map(|v| v.name) });
        }
        out.push(v);
    }
    Ok(json!({ "connection_id": c.id, "account_name": c.account_name, "cards": out }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ListCharges {
    vendor_id: Option<Uuid>,
    open: bool,
    limit: Option<i64>,
}

pub async fn list_charges(cx: &mut Cx<'_>, input: ListCharges) -> Result<Value, CommandError> {
    let limit = input.limit.unwrap_or(100).clamp(1, 500);
    let charges = cards::list_charges(&mut cx.tx, input.vendor_id, input.open, limit).await?;
    Ok(json!({ "charges": charges.into_iter().map(described).collect::<Vec<_>>() }))
}

/// A charge, with what is left for a person to do about it.
fn described(c: cards::CardCharge) -> Value {
    let needs = if c.status == "reversed" || c.dismissed_at.is_some() {
        None
    } else if c.bill_id.is_none() {
        Some(if c.status == "pending" {
            "pending at the bank; matched once it posts"
        } else {
            "pays no bill Payvy can tell: match it to a bill, or dismiss it"
        })
    } else if c.bill_state.as_deref() == Some("open") {
        Some("charged before the bill was released: the bill is marked paid once it is released")
    } else {
        None
    };
    let mut v = json!(c);
    v["needs"] = json!(needs);
    v
}

// ── Linking ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    vendor_id: Uuid,
    card_id: String,
    charges_from: Option<NaiveDate>,
    reason: Option<String>,
}

pub async fn link(cx: &mut Cx<'_>, input: Link) -> Result<Value, CommandError> {
    let vendor = store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let c = mercury_connection(cx).await?;
    // Ask the bank first, then take the lock and check nothing moved.
    let card = fetch_cards(cx, &c)
        .await?
        .into_iter()
        .find(|k| k.id == input.card_id)
        .ok_or(CommandError::NotFound("card at the connected bank"))?;
    if !matches!(card.status.as_str(), "active" | "frozen") {
        return Err(CommandError::Conflict(format!(
            "card ••{} is {}; a vendor can't charge it",
            card.last_four, card.status
        )));
    }
    store::lock_bank(&mut cx.tx, cx.org).await?;
    if store::live_paying_connection(&mut cx.tx)
        .await?
        .map(|x| x.id)
        != Some(c.id)
    {
        return Err(CommandError::Conflict(
            "the bank connection changed meanwhile; try again".into(),
        ));
    }
    if let Some(other) = cards::vendor_card_by_card(&mut cx.tx, c.id, &card.id).await?
        && other.vendor_id != vendor.id
    {
        let name = store::get_vendor(&mut cx.tx, other.vendor_id)
            .await?
            .map(|v| v.name)
            .unwrap_or_default();
        return Err(CommandError::Conflict(format!(
            "card ••{} is already on file with {name}; one card can't say which vendor a charge paid. \
             Remove it there first, or give this vendor its own card",
            card.last_four
        )));
    }
    let previous = cards::active_vendor_card(&mut cx.tx, vendor.id).await?;
    if let Some(p) = &previous
        && p.card_id == card.id
        && p.connection_id == c.id
    {
        return Err(CommandError::Conflict(format!(
            "card ••{} is already on file with {}",
            card.last_four, vendor.name
        )));
    }
    let today = cx.now.date_naive();
    let charges_from = input
        .charges_from
        .unwrap_or(today - Duration::days(DEFAULT_LOOKBACK_DAYS));
    if charges_from > today {
        return Err(CommandError::Invalid(
            "charges_from can't be in the future".into(),
        ));
    }
    if let Some(p) = &previous {
        cards::remove_vendor_card(&mut cx.tx, p.id, &cx.actor.name, cx.now).await?;
    }
    let reason = text(input.reason);
    let linked = cards::insert_vendor_card(
        &mut cx.tx,
        cx.org,
        &NewVendorCard {
            vendor_id: vendor.id,
            connection_id: c.id,
            card_id: &card.id,
            last_four: &card.last_four,
            nickname: card.nickname.as_deref(),
            kind: &card.kind,
            charges_from,
            reason: reason.as_deref(),
            linked_by: &cx.actor.name,
            linked_at: cx.now,
        },
    )
    .await?;
    let payee = store::active_payee(&mut cx.tx, vendor.id)
        .await?
        .filter(|p| p.connection_id == c.id);
    cx.emit(
        "vendor.card_linked",
        vendor.id,
        json!({ "vendor_id": vendor.id, "card": shown(&linked), "reason": reason,
                "replaced": previous.as_ref().map(shown),
                "transfers_stop": payee.is_some() }),
    )
    .await?;
    Ok(json!({
        "card": shown(&linked),
        "note": format!(
            "{} charges card ••{} itself. Releasing its bills sends no payment{}; Payvy reads the card's \
             charges from {} on and marks a bill paid when one plainly pays it.",
            vendor.name, linked.last_four,
            if payee.is_some() { " (its payee is no longer used while the card is on file)" } else { "" },
            charges_from,
        ),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unlink {
    vendor_id: Uuid,
    reason: String,
}

pub async fn unlink(cx: &mut Cx<'_>, input: Unlink) -> Result<Value, CommandError> {
    let reason = text(Some(input.reason))
        .ok_or_else(|| CommandError::Invalid("reason can't be empty".into()))?;
    store::lock_bank(&mut cx.tx, cx.org).await?;
    let k = cards::active_vendor_card(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("card on file for this vendor"))?;
    cards::remove_vendor_card(&mut cx.tx, k.id, &cx.actor.name, cx.now).await?;
    cx.emit(
        "vendor.card_removed",
        k.vendor_id,
        json!({ "vendor_id": k.vendor_id, "card": shown(&k), "reason": reason }),
    )
    .await?;
    Ok(json!({ "removed": shown(&k),
               "note": "Charges already seen stay. Releasing this vendor's bills pays its payee again, if it has one." }))
}

// ── Reading charges and matching them ──────────────────────────────────────

fn timestamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Sync {}

/// Reads the charges on every card on file from the bank, records them,
/// pairs the plain ones with bills and marks cleared bills paid. The
/// payment worker runs it every few minutes as the agent `bank`; anyone
/// may run it to check now.
pub async fn sync(cx: &mut Cx<'_>, _input: Sync) -> Result<Value, CommandError> {
    let Some(c) = store::live_paying_connection(&mut cx.tx)
        .await?
        .filter(|c| c.provider == "mercury")
    else {
        return Ok(json!({ "cards": 0, "note": "no Mercury connection" }));
    };
    let links = cards::vendor_cards_at(&mut cx.tx, c.id).await?;
    let Some(since) = links.iter().map(|k| k.charges_from).min() else {
        return Ok(json!({ "cards": 0 }));
    };
    let ids: Vec<String> = links.iter().map(|k| k.card_id.clone()).collect();
    let seen = bank::mercury(cx, &c)?
        .card_transactions(&ids, &since.to_string())
        .await?;

    // The bank has answered; from here on, one sync at a time per company.
    store::lock_bank(&mut cx.tx, cx.org).await?;
    let links: HashMap<String, VendorCard> = cards::vendor_cards_at(&mut cx.tx, c.id)
        .await?
        .into_iter()
        .map(|k| (k.card_id.clone(), k))
        .collect();
    let mut recorded = 0;
    let mut vendors = BTreeSet::new();
    for t in &seen {
        // Charges only: a refund pays no bill.
        let (Some(k), true) = (links.get(&t.card_id), t.amount_cents < 0) else {
            continue;
        };
        let Some(charged_at) = timestamp(&t.created_at) else {
            continue;
        };
        if charged_at.date_naive() < k.charges_from {
            continue;
        }
        vendors.insert(k.vendor_id);
        let status = ChargeStatus::from_mercury(&t.status);
        let (id, before) = cards::record_charge(
            &mut cx.tx,
            cx.org,
            &SeenCharge {
                connection_id: c.id,
                vendor_card_id: k.id,
                vendor_id: k.vendor_id,
                transaction_id: &t.id,
                amount_cents: -t.amount_cents,
                status: status.as_str(),
                counterparty: t.counterparty.as_deref(),
                charged_at,
                posted_at: t.posted_at.as_deref().and_then(timestamp),
                link: t.link.as_deref(),
            },
            cx.now,
        )
        .await?;
        if before.as_deref() == Some(status.as_str()) {
            continue;
        }
        recorded += 1;
        let charge = cards::get_charge(&mut cx.tx, id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("charge {id} vanished"))?;
        cx.emit(
            &format!("card.charge_{}", status.as_str()),
            k.vendor_id,
            json!({ "charge_id": id, "vendor_id": k.vendor_id, "card": format!("••{}", k.last_four),
                    "amount_cents": charge.amount_cents, "transaction_id": t.id,
                    "from": before, "to": status.as_str(), "bill_id": charge.bill_id }),
        )
        .await?;
        if status == ChargeStatus::Reversed {
            reversed(cx, &charge).await?;
        }
    }
    let mut matched = 0;
    let mut paid = 0;
    for vendor_id in vendors {
        matched += match_vendor(cx, vendor_id).await?;
        paid += settle_vendor(cx, vendor_id).await?;
    }
    Ok(
        json!({ "cards": links.len(), "charges_seen": seen.len(), "recorded": recorded,
               "matched": matched, "paid": paid }),
    )
}

/// A charge that paid a bill and then didn't go (cancelled, reversed):
/// the bill is open again and waits for a person, like a returned ACH.
async fn reversed(cx: &mut Cx<'_>, charge: &cards::CardCharge) -> Result<(), CommandError> {
    let Some(bill_id) = charge.bill_id else {
        return Ok(());
    };
    cards::set_charge_bill(&mut cx.tx, charge.id, None, None, None, None).await?;
    let bill = store::get_bill(&mut cx.tx, bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    if bill.state == payvy_domain::bills::State::Paid
        && bill.paid_reference.as_deref() == Some(charge.transaction_id.as_str())
    {
        store::reopen_returned_bill(&mut cx.tx, bill.id).await?;
        cx.emit(
            "bill.payment_returned",
            bill.id,
            json!({ "bill_id": bill.id, "charge_id": charge.id, "via": "card",
                    "failure": "the card charge was reversed or cancelled at the bank" }),
        )
        .await?;
    }
    Ok(())
}

/// Pairs this vendor's unmatched posted charges with its open bills, where
/// plain. Returns how many it paired.
async fn match_vendor(cx: &mut Cx<'_>, vendor_id: Uuid) -> Result<usize, CommandError> {
    let charges = cards::unmatched_posted_charges(&mut cx.tx, vendor_id).await?;
    if charges.is_empty() {
        return Ok(0);
    }
    let bills = cards::bills_awaiting_a_charge(&mut cx.tx, vendor_id).await?;
    let pairs = plain_matches(
        &charges
            .iter()
            .map(|c| OpenCharge {
                amount_cents: c.amount_cents,
                charged_on: c.charged_at.date_naive(),
            })
            .collect::<Vec<_>>(),
        &bills
            .iter()
            .map(|(_, cents, on)| OpenBill {
                amount_cents: *cents,
                invoice_date: *on,
            })
            .collect::<Vec<_>>(),
    );
    for (ci, bi) in &pairs {
        let charge = &charges[*ci];
        let bill_id = bills[*bi].0;
        cards::set_charge_bill(
            &mut cx.tx,
            charge.id,
            Some(bill_id),
            Some("amount"),
            Some(MATCHER),
            Some(cx.now),
        )
        .await?;
        cx.emit_as(
            MATCHER,
            "card.charge_matched",
            bill_id,
            json!({ "charge_id": charge.id, "bill_id": bill_id, "rule": "amount",
                    "amount_cents": charge.amount_cents, "card": format!("••{}", charge.last_four),
                    "transaction_id": charge.transaction_id, "triggered_by": cx.actor.name }),
        )
        .await?;
    }
    Ok(pairs.len())
}

/// Marks paid every cleared bill of this vendor that a posted charge pays.
async fn settle_vendor(cx: &mut Cx<'_>, vendor_id: Uuid) -> Result<usize, CommandError> {
    let mut paid = 0;
    for charge in cards::charges_paying_open_bills(&mut cx.tx, vendor_id).await? {
        if super::bills::settle_by_card(cx, &charge).await? {
            paid += 1;
        }
    }
    Ok(paid)
}

/// After a card vendor's bill is released: if a posted charge already pays
/// it, it is paid now.
pub(super) async fn settle_bill(cx: &mut Cx<'_>, bill_id: Uuid) -> Result<bool, CommandError> {
    match cards::charge_for_bill(&mut cx.tx, bill_id).await? {
        Some(c) if c.status == "posted" => super::bills::settle_by_card(cx, &c).await,
        _ => Ok(false),
    }
}

// ── A person decides ────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchCharge {
    charge_id: Uuid,
    bill_id: Uuid,
}

pub async fn match_charge(cx: &mut Cx<'_>, input: MatchCharge) -> Result<Value, CommandError> {
    store::lock_bank(&mut cx.tx, cx.org).await?;
    let charge = cards::get_charge(&mut cx.tx, input.charge_id)
        .await?
        .ok_or(CommandError::NotFound("charge"))?;
    if charge.status == "reversed" {
        return Err(CommandError::Conflict(
            "this charge was reversed at the bank; it pays nothing".into(),
        ));
    }
    if charge.dismissed_at.is_some() {
        return Err(CommandError::Conflict("this charge was dismissed".into()));
    }
    if let Some(b) = charge.bill_id {
        return Err(CommandError::Conflict(format!(
            "this charge already pays bill {b}"
        )));
    }
    let bill = store::get_bill(&mut cx.tx, input.bill_id)
        .await?
        .ok_or(CommandError::NotFound("bill"))?;
    if bill.vendor_id != charge.vendor_id {
        return Err(CommandError::Invalid(format!(
            "this charge is on {}'s card; the bill is {}'s",
            charge.vendor_name, bill.vendor_name
        )));
    }
    if bill.state != payvy_domain::bills::State::Open {
        return Err(CommandError::Conflict(format!(
            "this bill is {}",
            bill.state.as_str()
        )));
    }
    if bill.amount_cents != charge.amount_cents {
        return Err(CommandError::Invalid(format!(
            "the charge is {} and the bill is {}: one charge pays one whole bill. If the vendor charged \
             a different amount, correct the bill first",
            payvy_domain::workflow::dollars(charge.amount_cents),
            payvy_domain::workflow::dollars(bill.amount_cents)
        )));
    }
    if let Some(other) = cards::charge_for_bill(&mut cx.tx, bill.id).await? {
        return Err(CommandError::Conflict(format!(
            "this bill is already paid by the {} charge of {}",
            payvy_domain::workflow::dollars(other.amount_cents),
            other.charged_at.date_naive()
        )));
    }
    cards::set_charge_bill(
        &mut cx.tx,
        charge.id,
        Some(bill.id),
        Some("person"),
        Some(&cx.actor.name),
        Some(cx.now),
    )
    .await?;
    cx.emit(
        "card.charge_matched",
        bill.id,
        json!({ "charge_id": charge.id, "bill_id": bill.id, "rule": "person",
                "amount_cents": charge.amount_cents, "card": format!("••{}", charge.last_four),
                "transaction_id": charge.transaction_id }),
    )
    .await?;
    let paid = settle_bill(cx, bill.id).await?;
    let now = cards::get_charge(&mut cx.tx, charge.id)
        .await?
        .ok_or(CommandError::NotFound("charge"))?;
    Ok(json!({ "charge": described(now), "bill_paid": paid,
               "next": if paid { None } else { Some("the bill is marked paid once it is released and the charge has posted") } }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dismiss {
    charge_id: Uuid,
    reason: String,
}

pub async fn dismiss(cx: &mut Cx<'_>, input: Dismiss) -> Result<Value, CommandError> {
    let reason = text(Some(input.reason))
        .ok_or_else(|| CommandError::Invalid("reason can't be empty".into()))?;
    store::lock_bank(&mut cx.tx, cx.org).await?;
    let charge = cards::get_charge(&mut cx.tx, input.charge_id)
        .await?
        .ok_or(CommandError::NotFound("charge"))?;
    if let Some(b) = charge.bill_id {
        return Err(CommandError::Conflict(format!(
            "this charge pays bill {b}; it can't be dismissed"
        )));
    }
    if charge.dismissed_at.is_some() {
        return Err(CommandError::Conflict(
            "this charge is already dismissed".into(),
        ));
    }
    cards::dismiss_charge(&mut cx.tx, charge.id, &cx.actor.name, &reason, cx.now).await?;
    cx.emit(
        "card.charge_dismissed",
        charge.vendor_id,
        json!({ "charge_id": charge.id, "vendor_id": charge.vendor_id, "reason": reason,
                "amount_cents": charge.amount_cents, "transaction_id": charge.transaction_id }),
    )
    .await?;
    let now = cards::get_charge(&mut cx.tx, charge.id)
        .await?
        .ok_or(CommandError::NotFound("charge"))?;
    Ok(json!({ "charge": described(now) }))
}
