//! PO lifecycle and the allocation ledger (PO Matching Spec v2, step 1),
//! through the real command dispatcher against a real database.

use payvy_server::Actor;
use payvy_server::commands::CommandError;
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, bill, commit, confirm, person, vendor};

/// Purchase orders are off until an admin turns them on.
async fn po_on(db: &TestDb) {
    commit(
        &db.state,
        &person("admin"),
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
}

/// A person who signs in with only the roles given (not a bootstrap admin).
async fn add_person(db: &TestDb, actor: &str, roles: Value) -> Actor {
    commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": format!("{actor}@example.com"), "name": actor, "actor": actor, "roles": roles }),
    )
    .await
    .unwrap();
    Actor::person(payvy_store::DEFAULT_ORG, actor)
}

/// The PO owner must be a person in the company before activation.
async fn add_robert(db: &TestDb) {
    let r = commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": "robert@example.com", "name": "Robert", "actor": "robert" }),
    )
    .await;
    if let Err(e) = r {
        assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    }
}

async fn active_po(db: &TestDb, vendor_id: &str, number: &str, cents: i64) -> String {
    add_robert(db).await;
    let robert = person("robert");
    let created = commit(&db.state, &agent("claude-ap"), "po.create", json!({
        "vendor_id": vendor_id, "po_number": number, "owner": "robert", "authorized_cents": cents,
    })).await.unwrap();
    let id = created["result"]["id"].as_str().unwrap().to_owned();
    commit(&db.state, &robert, "po.activate", json!({ "po_id": id }))
        .await
        .unwrap();
    id
}

async fn amounts(db: &TestDb, po_id: &str) -> Value {
    commit(
        &db.state,
        &person("robert"),
        "po.get",
        json!({ "po_id": po_id }),
    )
    .await
    .unwrap()["result"]["amounts"]
        .clone()
}

#[tokio::test]
async fn lifecycle_draft_then_person_activates_and_agents_cannot() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Acme Services").await;
    // No owner yet: activation is refused.
    let created = commit(
        &db.state,
        &agent("claude-ap"),
        "po.create",
        json!({
            "vendor_id": v, "po_number": "PO-1", "authorized_cents": 100_000,
        }),
    )
    .await
    .unwrap();
    assert_eq!(created["result"]["status"], "draft");
    let id = created["result"]["id"].as_str().unwrap().to_owned();
    let e = commit(&db.state, &robert, "po.activate", json!({ "po_id": id }))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    // An agent cannot activate even once an owner exists.
    commit(
        &db.state,
        &robert,
        "po.update",
        json!({ "po_id": id, "owner": "robert" }),
    )
    .await
    .unwrap();
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "po.activate",
        json!({ "po_id": id }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    // The owner must be a person in the company, not just a name.
    let e = commit(&db.state, &robert, "po.activate", json!({ "po_id": id }))
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("isn't a person in this company"),
        "{e}"
    );
    add_robert(&db).await;
    let active = commit(&db.state, &robert, "po.activate", json!({ "po_id": id }))
        .await
        .unwrap();
    assert_eq!(active["result"]["status"], "active");
    assert_eq!(active["result"]["activated_by"], "robert");
    assert_eq!(active["result"]["amounts"]["progress"], "unbilled");
    db.drop().await;
}

#[tokio::test]
async fn partial_billing_reserves_accepts_and_reverses() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Acme Services").await;
    let po = active_po(&db, &v, "PO-100", 100_000).await;
    let b400 = bill(&db.state, &agent("claude-ap"), &v, "inv-400", 40_000).await;
    // A preview reserves nothing.
    let preview = commit(
        &db.state,
        &agent("claude-ap"),
        "po.match_preview",
        json!({ "bill_id": b400 }),
    )
    .await
    .unwrap();
    assert_eq!(preview["result"]["candidates"][0]["proposed_cents"], 40_000);
    assert_eq!(amounts(&db, &po).await["reserved_cents"], 0);
    // $400 then $600 against a $1,000 PO: both valid, neither equals the total.
    let applied = confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": b400, "allocations": [{ "po_id": po, "amount_cents": 40_000 }],
        }),
    )
    .await
    .unwrap();
    assert_eq!(applied["result"]["uncovered_cents"], 0);
    let a = amounts(&db, &po).await;
    assert_eq!(a["reserved_cents"], 40_000);
    assert_eq!(a["available_cents"], 60_000);
    assert_eq!(a["progress"], "partially_billed");
    // Reserving is not approval and not consumption.
    let alloc_id = applied["result"]["allocations"][0]["allocation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let accepted = commit(
        &db.state,
        &agent("claude-ap"),
        "po.allocation_accept",
        json!({ "allocation_id": alloc_id }),
    )
    .await
    .unwrap();
    assert_eq!(accepted["result"]["amounts"]["consumed_cents"], 40_000);
    assert_eq!(accepted["result"]["amounts"]["reserved_cents"], 0);
    let b600 = bill(&db.state, &agent("claude-ap"), &v, "inv-600", 60_000).await;
    confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": b600, "allocations": [{ "po_id": po, "amount_cents": 60_000 }],
        }),
    )
    .await
    .unwrap();
    assert_eq!(amounts(&db, &po).await["progress"], "fully_billed");
    // Nothing left: even $1 is refused, and releasing restores it exactly.
    let b1 = bill(&db.state, &agent("claude-ap"), &v, "inv-1", 100).await;
    let e = confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": b1, "allocations": [{ "po_id": po, "amount_cents": 100 }],
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    let got = commit(&db.state, &robert, "po.get", json!({ "po_id": po }))
        .await
        .unwrap();
    let alloc600 = got["result"]["allocations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["amount_cents"] == 60_000)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    commit(
        &db.state,
        &agent("claude-ap"),
        "po.allocation_reverse",
        json!({
            "allocation_id": alloc600, "reason": "bill rejected",
        }),
    )
    .await
    .unwrap();
    assert_eq!(amounts(&db, &po).await["available_cents"], 60_000);
    // Releasing twice does not restore twice.
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "po.allocation_reverse",
        json!({
            "allocation_id": alloc600, "reason": "again",
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert_eq!(amounts(&db, &po).await["available_cents"], 60_000);
    db.drop().await;
}

#[tokio::test]
async fn overage_cap_is_cumulative_smaller_of_both_and_revision_pinned() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Acme Services").await;
    commit(&db.state, &robert, "po.set_vendor_settings", json!({
        "vendor_id": v, "require_po": true, "overage_cents_cap": 2_500, "overage_percent_bp": 500,
    })).await.unwrap();
    let po = active_po(&db, &v, "PO-CAP", 100_000).await;
    let got = commit(&db.state, &robert, "po.get", json!({ "po_id": po }))
        .await
        .unwrap();
    // $25 and 5% caps on $1,000: the smaller ($25) governs; ceiling $1,025.
    assert_eq!(got["result"]["revision"]["allowance_cents"], 2_500);
    let b1 = bill(&db.state, &agent("claude-ap"), &v, "cap-1", 60_000).await;
    let b2 = bill(&db.state, &agent("claude-ap"), &v, "cap-2", 60_000).await;
    confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": b1, "allocations": [{ "po_id": po, "amount_cents": 60_000 }],
        }),
    )
    .await
    .unwrap();
    // Splitting the overcharge across invoices cannot evade the cumulative cap.
    let e = confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": b2, "allocations": [{ "po_id": po, "amount_cents": 60_000 }],
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    let b3 = bill(&db.state, &agent("claude-ap"), &v, "cap-3", 42_500).await;
    confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": b3, "allocations": [{ "po_id": po, "amount_cents": 42_500 }],
        }),
    )
    .await
    .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn a_revision_never_releases_allocations_and_splits_are_atomic() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Acme Services").await;
    let a = active_po(&db, &v, "PO-A", 30_000).await;
    let b = active_po(&db, &v, "PO-B", 30_000).await;
    let bill600 = bill(&db.state, &agent("claude-ap"), &v, "split-1", 60_000).await;
    confirm(&db.state, &agent("claude-ap"), "po.match_apply", json!({
        "bill_id": bill600,
        "allocations": [{ "po_id": a, "amount_cents": 30_000 }, { "po_id": b, "amount_cents": 30_000 }],
    })).await.unwrap();
    // Revising A upwards keeps its reservation and its original revision.
    let before = commit(&db.state, &robert, "po.get", json!({ "po_id": a }))
        .await
        .unwrap();
    let rev1_alloc = before["result"]["allocations"][0].clone();
    commit(
        &db.state,
        &robert,
        "po.update",
        json!({
            "po_id": a, "authorized_cents": 200_000, "reason": "scope grew",
        }),
    )
    .await
    .unwrap();
    let after = amounts(&db, &a).await;
    assert_eq!(after["reserved_cents"], 30_000);
    assert_eq!(after["authorized_cents"], 200_000);
    let after_get = commit(&db.state, &robert, "po.get", json!({ "po_id": a }))
        .await
        .unwrap();
    assert_eq!(
        after_get["result"]["allocations"][0]["revision_id"],
        rev1_alloc["revision_id"]
    );
    assert_eq!(after_get["result"]["revision"]["revision"], 2);
    // A failing split reserves on neither PO.
    let other_bill = bill(&db.state, &agent("claude-ap"), &v, "split-2", 40_000).await;
    let e = confirm(&db.state, &agent("claude-ap"), "po.match_apply", json!({
        "bill_id": other_bill,
        "allocations": [{ "po_id": a, "amount_cents": 20_000 }, { "po_id": b, "amount_cents": 20_000 }],
    })).await.unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert_eq!(amounts(&db, &a).await["reserved_cents"], 30_000);
    assert_eq!(amounts(&db, &b).await["reserved_cents"], 30_000);
    // And allocations may not exceed the bill itself.
    let small = bill(&db.state, &agent("claude-ap"), &v, "split-3", 10_000).await;
    let e = confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": small, "allocations": [{ "po_id": a, "amount_cents": 20_000 }],
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn imports_land_as_drafts_with_opening_consumption_counted_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Acme Services").await;
    let input = json!({
        "vendor_id": v, "po_number": "EXT-9", "external_source": "visibooks", "external_id": "c-9",
        "owner": "robert", "authorized_cents": 100_000,
        "opening_consumed_cents": 40_000, "source_allocation_id": "alloc-1",
    });
    let imported = commit(&db.state, &agent("importer"), "po.import", input.clone())
        .await
        .unwrap();
    assert_eq!(imported["result"]["status"], "draft");
    assert_eq!(imported["result"]["activated_by"], Value::Null);
    let id = imported["result"]["id"].as_str().unwrap().to_owned();
    // Drafts take no allocations, and re-importing does not double-count.
    let b = bill(&db.state, &agent("claude-ap"), &v, "imp-1", 10_000).await;
    let e = confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": b, "allocations": [{ "po_id": id, "amount_cents": 10_000 }],
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    let again = commit(&db.state, &agent("importer"), "po.import", input)
        .await
        .unwrap();
    assert_eq!(again["result"]["id"], id);
    add_robert(&db).await;
    commit(&db.state, &robert, "po.activate", json!({ "po_id": id }))
        .await
        .unwrap();
    let a = amounts(&db, &id).await;
    assert_eq!(a["consumed_cents"], 40_000);
    assert_eq!(a["available_cents"], 60_000);
    db.drop().await;
}

#[tokio::test]
async fn two_bills_racing_for_the_last_capacity_exactly_one_reserves() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Acme Services").await;
    let po = active_po(&db, &v, "PO-RACE", 100_000).await;
    let b1 = bill(&db.state, &agent("claude-ap"), &v, "race-1", 70_000).await;
    let b2 = bill(&db.state, &agent("claude-ap"), &v, "race-2", 70_000).await;
    let apply =
        |bill_id: String| {
            let state = &db.state;
            let po = po.clone();
            async move {
                confirm(state, &agent("claude-ap"), "po.match_apply", json!({
                "bill_id": bill_id, "allocations": [{ "po_id": po, "amount_cents": 70_000 }],
            })).await
            }
        };
    let (r1, r2) = tokio::join!(apply(b1), apply(b2));
    assert_eq!([r1.is_ok(), r2.is_ok()].iter().filter(|ok| **ok).count(), 1);
    assert_eq!(amounts(&db, &po).await["reserved_cents"], 70_000);
    db.drop().await;
}

async fn manual_po_vendor(db: &TestDb) -> String {
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Gated Hauler").await;
    common::set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    commit(
        &db.state,
        &robert,
        "po.set_vendor_settings",
        json!({
            "vendor_id": v, "require_po": true,
        }),
    )
    .await
    .unwrap();
    v
}

#[tokio::test]
async fn the_payment_gate_blocks_release_and_an_override_permits_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = manual_po_vendor(&db).await;
    let b = bill(&db.state, &agent("claude-ap"), &v, "gate-1", 50_000).await;
    let got = commit(&db.state, &robert, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(
        got["result"]["purchase_orders"]["exception"]["kind"],
        "no_po"
    );
    assert_eq!(got["result"]["purchase_orders"]["uncovered_cents"], 50_000);
    // Release is refused while the exception stands.
    let e = common::confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("no_po"), "{e}");
    // mass assignment of bank details is not the override: only a person, with a reason.
    let e = confirm(
        &db.state,
        &agent("claude-ap"),
        "po.override",
        json!({
            "bill_id": b, "authorized_excess_cents": 50_000, "reason": "one-off",
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    // An override below the excess is refused; at the excess it permits release.
    let e = confirm(
        &db.state,
        &robert,
        "po.override",
        json!({
            "bill_id": b, "authorized_excess_cents": 49_999, "reason": "one-off",
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    let over = confirm(&db.state, &robert, "po.override", json!({
        "bill_id": b, "authorized_excess_cents": 50_000, "reason": "verbal PO, paperwork follows",
    })).await.unwrap();
    assert_eq!(over["result"]["overridden"], true);
    // Whoever permitted the exception doesn't release; someone else does.
    common::confirm(
        &db.state,
        &person("alice"),
        "bill.release",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn a_relevant_change_voids_an_override_and_reject_releases_allocations() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = manual_po_vendor(&db).await;
    let po = active_po(&db, &v, "PO-GATE", 100_000).await;
    let b = bill(&db.state, &agent("claude-ap"), &v, "gate-2", 50_000).await;
    confirm(
        &db.state,
        &robert,
        "po.override",
        json!({
            "bill_id": b, "authorized_excess_cents": 50_000, "reason": "paperwork follows",
        }),
    )
    .await
    .unwrap();
    // Allocating after the override changes the scoped facts: it no longer holds.
    confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": b, "allocations": [{ "po_id": po, "amount_cents": 10_000 }],
        }),
    )
    .await
    .unwrap();
    let e = common::confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("uncovered"), "{e}");
    // Rejecting the bill releases its allocation by reversal.
    commit(
        &db.state,
        &robert,
        "bill.reject",
        json!({ "bill_id": b, "reason": "not ours" }),
    )
    .await
    .unwrap();
    assert_eq!(amounts(&db, &po).await["reserved_cents"], 0);
    assert_eq!(amounts(&db, &po).await["available_cents"], 100_000);
    db.drop().await;
}

#[tokio::test]
async fn the_gate_blocks_auto_pay_but_mark_paid_only_warns() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Auto Gated").await;
    common::set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "auto" }]),
    )
    .await;
    commit(
        &db.state,
        &robert,
        "po.set_vendor_settings",
        json!({
            "vendor_id": v, "require_po": true,
        }),
    )
    .await
    .unwrap();
    let b = bill(&db.state, &agent("claude-ap"), &v, "gate-3", 10_000).await;
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "bill.auto_pay",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("no_po"), "{e}");
    // mark_paid records a payment that already happened: it warns, not blocks.
    let paid = common::confirm(
        &db.state,
        &robert,
        "bill.mark_paid",
        json!({
            "bill_id": b, "paid_on": "2027-01-20", "method": "ach",
        }),
    )
    .await
    .unwrap();
    assert!(
        paid["result"]["po_warning"]
            .as_str()
            .unwrap()
            .contains("no_po")
    );
    db.drop().await;
}

#[tokio::test]
async fn the_preparer_cannot_override_and_the_overrider_cannot_release() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let v = manual_po_vendor(&db).await;
    // Pam prepares the bill; Olga (an admin) permits its exception; Rita
    // (a payment releaser) releases it.
    let pam = add_person(&db, "pam", json!(["admin"])).await;
    let olga = add_person(&db, "olga", json!(["admin"])).await;
    let rita = add_person(&db, "rita", json!(["payment_releaser"])).await;
    let b = bill(&db.state, &pam, &v, "sod-1", 30_000).await;
    let input = json!({ "bill_id": b, "authorized_excess_cents": 30_000, "reason": "verbal PO" });
    let e = confirm(&db.state, &pam, "po.override", input.clone())
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Forbidden(_)), "{e}");
    assert!(e.to_string().contains("You prepared this bill"), "{e}");
    let got = commit(&db.state, &pam, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(got["result"]["purchase_orders"]["can_override"], false);
    let over = confirm(&db.state, &olga, "po.override", input)
        .await
        .unwrap();
    assert_eq!(over["result"]["overridden"], true);
    assert_eq!(over["result"]["overridden_by"], "olga");
    // The overrider is told up front, and refused if she tries.
    let got = commit(&db.state, &olga, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(got["result"]["can_release"], false);
    let e = confirm(&db.state, &olga, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Forbidden(_)), "{e}");
    assert!(
        e.to_string()
            .contains("You permitted this bill's PO exception"),
        "{e}"
    );
    let got = commit(&db.state, &rita, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(got["result"]["can_release"], true);
    confirm(&db.state, &rita, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn an_overrider_may_release_a_bill_that_no_longer_needs_the_override() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let v = manual_po_vendor(&db).await;
    let olga = add_person(&db, "olga", json!(["admin"])).await;
    let po = active_po(&db, &v, "PO-SOD", 100_000).await;
    let b = bill(&db.state, &agent("claude-ap"), &v, "sod-2", 20_000).await;
    confirm(
        &db.state,
        &olga,
        "po.override",
        json!({
            "bill_id": b, "authorized_excess_cents": 20_000, "reason": "paperwork follows",
        }),
    )
    .await
    .unwrap();
    // Fully matched now: the override isn't what makes it payable.
    confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": b, "allocations": [{ "po_id": po, "amount_cents": 20_000 }],
        }),
    )
    .await
    .unwrap();
    confirm(&db.state, &olga, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn purchase_orders_are_off_by_default_and_only_a_person_admin_turns_them_on() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Acme Services").await;
    let s = commit(&db.state, &agent("claude-ap"), "po.settings", json!({}))
        .await
        .unwrap();
    assert_eq!(s["result"]["enabled"], false);
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "po.create",
        json!({
            "vendor_id": v, "po_number": "PO-OFF", "authorized_cents": 1_000,
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("turned off"), "{e}");
    let e = commit(
        &db.state,
        &robert,
        "po.set_vendor_settings",
        json!({
            "vendor_id": v, "require_po": true,
        }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("turned off"), "{e}");
    // Reads still work.
    commit(&db.state, &robert, "po.list", json!({}))
        .await
        .unwrap();
    commit(
        &db.state,
        &robert,
        "po.vendor_settings",
        json!({ "vendor_id": v }),
    )
    .await
    .unwrap();
    // Only a person, and only an admin, turns them on.
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let rita = add_person(&db, "rita", json!(["payment_releaser", "ap_clerk"])).await;
    let e = commit(&db.state, &rita, "po.configure", json!({ "enabled": true }))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Forbidden(_)), "{e}");
    let on = commit(
        &db.state,
        &robert,
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
    assert_eq!(on["result"]["enabled"], true);
    assert_eq!(on["result"]["updated_by"], "robert");
    assert!(
        on["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "po.settings_changed")
    );
    // A no-op is refused.
    let e = commit(
        &db.state,
        &robert,
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    commit(
        &db.state,
        &agent("claude-ap"),
        "po.create",
        json!({
            "vendor_id": v, "po_number": "PO-ON", "authorized_cents": 1_000,
        }),
    )
    .await
    .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn turning_purchase_orders_off_lifts_the_gate_and_reject_still_releases() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = manual_po_vendor(&db).await;
    let po = active_po(&db, &v, "PO-OFF", 100_000).await;
    let blocked = bill(&db.state, &agent("claude-ap"), &v, "off-1", 50_000).await;
    let e = confirm(
        &db.state,
        &robert,
        "bill.release",
        json!({ "bill_id": blocked }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("no_po"), "{e}");
    let matched = bill(&db.state, &agent("claude-ap"), &v, "off-2", 40_000).await;
    confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({
            "bill_id": matched, "allocations": [{ "po_id": po, "amount_cents": 40_000 }],
        }),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &robert,
        "po.configure",
        json!({ "enabled": false }),
    )
    .await
    .unwrap();
    // Writes are refused, the bill page hides the section, and the gate lifts.
    let e = confirm(
        &db.state,
        &robert,
        "po.override",
        json!({
            "bill_id": blocked, "authorized_excess_cents": 50_000, "reason": "x",
        }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("turned off"), "{e}");
    let got = commit(
        &db.state,
        &robert,
        "bill.get",
        json!({ "bill_id": blocked }),
    )
    .await
    .unwrap();
    assert_eq!(got["result"]["purchase_orders"]["enabled"], false);
    assert_eq!(got["result"]["purchase_orders"]["exception"], Value::Null);
    confirm(
        &db.state,
        &robert,
        "bill.release",
        json!({ "bill_id": blocked }),
    )
    .await
    .unwrap();
    // Rejecting a bill still returns its reserved capacity.
    assert_eq!(amounts(&db, &po).await["reserved_cents"], 40_000);
    commit(
        &db.state,
        &robert,
        "bill.reject",
        json!({ "bill_id": matched, "reason": "not ours" }),
    )
    .await
    .unwrap();
    assert_eq!(amounts(&db, &po).await["reserved_cents"], 0);
    assert_eq!(amounts(&db, &po).await["available_cents"], 100_000);
    db.drop().await;
}

#[tokio::test]
async fn customer_funding_blocks_release_until_the_signal_arrives() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let robert = person("robert");
    commit(
        &db.state,
        &robert,
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
    let v = vendor(&db.state, &robert, "Funded Hauler").await;
    common::set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    commit(
        &db.state,
        &robert,
        "po.set_vendor_settings",
        json!({ "vendor_id": v, "require_po": false, "require_funding": true }),
    )
    .await
    .unwrap();
    let b = bill(&db.state, &agent("claude-ap"), &v, "fund-1", 25_000).await;
    // Without the signal: the exception shows beside the bill and release is refused.
    let got = commit(&db.state, &robert, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(
        got["result"]["purchase_orders"]["exception"]["kind"],
        "funding_not_verified"
    );
    let e = common::confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("funding_not_verified"), "{e}");
    // The customer_funded signal is a signal, not verified payment; with it, release goes.
    commit(
        &db.state,
        &agent("claude-ap"),
        "bill.signal",
        json!({ "bill_id": b, "signal": "customer_funded" }),
    )
    .await
    .unwrap();
    let got = commit(&db.state, &robert, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(got["result"]["purchase_orders"]["exception"], Value::Null);
    common::confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn activation_refuses_opening_consumption_over_authorized_plus_allowance() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let robert = person("robert");
    add_robert(&db).await;
    commit(
        &db.state,
        &robert,
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
    let v = vendor(&db.state, &robert, "Imported Services").await;
    // Opening consumption of $1,100 against a $1,000 PO, no allowance: refuse.
    let over = commit(
        &db.state,
        &agent("importer"),
        "po.import",
        json!({
            "vendor_id": v, "po_number": "EXT-OVER", "external_source": "visibooks",
            "external_id": "c-over", "owner": "robert", "authorized_cents": 100_000,
            "opening_consumed_cents": 110_000, "source_allocation_id": "alloc-over",
        }),
    )
    .await
    .unwrap();
    let over_id = over["result"]["id"].as_str().unwrap().to_owned();
    let e = commit(
        &db.state,
        &robert,
        "po.activate",
        json!({ "po_id": over_id }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("opening consumption"), "{e}");
    assert!(e.to_string().contains("$1,100.00"), "{e}");
    assert!(!e.to_string().contains("110000"), "{e}");
    // Still a draft, and raising the authorized amount by revision fixes it.
    commit(
        &db.state,
        &robert,
        "po.update",
        json!({ "po_id": over_id, "authorized_cents": 120_000, "reason": "budget corrected" }),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &robert,
        "po.activate",
        json!({ "po_id": over_id }),
    )
    .await
    .unwrap();

    // With a vendor allowance pinned on the revision, over-authorized history
    // within the allowance activates: $1,100 consumed, $1,000 + $200 allowance.
    commit(
        &db.state,
        &robert,
        "po.set_vendor_settings",
        json!({ "vendor_id": v, "require_po": false, "overage_cents_cap": 20_000 }),
    )
    .await
    .unwrap();
    let within = commit(
        &db.state,
        &agent("importer"),
        "po.import",
        json!({
            "vendor_id": v, "po_number": "EXT-WITHIN", "external_source": "visibooks",
            "external_id": "c-within", "owner": "robert", "authorized_cents": 100_000,
            "opening_consumed_cents": 110_000, "source_allocation_id": "alloc-within",
        }),
    )
    .await
    .unwrap();
    let within_id = within["result"]["id"].as_str().unwrap().to_owned();
    let active = commit(
        &db.state,
        &robert,
        "po.activate",
        json!({ "po_id": within_id }),
    )
    .await
    .unwrap();
    assert_eq!(active["result"]["status"], "active");
    assert_eq!(active["result"]["amounts"]["consumed_cents"], 110_000);
    db.drop().await;
}

#[tokio::test]
async fn the_overrider_is_not_shown_the_bill_as_waiting_on_them() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let robert = person("robert");
    let olga = person("olga");
    po_on(&db).await;
    let v = manual_po_vendor(&db).await;
    let b = bill(&db.state, &agent("claude-ap"), &v, "gate-list-1", 50_000).await;
    common::confirm(
        &db.state,
        &olga,
        "po.override",
        json!({
            "bill_id": b, "authorized_excess_cents": 50_000,
            "reason": "paperwork follows",
        }),
    )
    .await
    .unwrap();

    // bill.get already refuses Olga the release; the lists must agree.
    let got_olga = commit(&db.state, &olga, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(got_olga["result"]["can_release"], false);
    let got_robert = commit(&db.state, &robert, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(got_robert["result"]["can_release"], true);

    // Waiting-on-you (bill.list, mine): Olga's list does not carry it as
    // hers; Robert's does, with can_release true.
    let mine_olga = commit(&db.state, &olga, "bill.list", json!({ "mine": true }))
        .await
        .unwrap();
    assert!(
        mine_olga["result"]["bills"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["bill"]["id"] != b),
        "the overrider must not see the bill as waiting on them"
    );
    let mine_robert = commit(&db.state, &robert, "bill.list", json!({ "mine": true }))
        .await
        .unwrap();
    let item = mine_robert["result"]["bills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["bill"]["id"] == b)
        .expect("another releaser still sees the bill as theirs");
    assert_eq!(item["can_release"], true);
    assert_eq!(item["overridden_by"], "olga");

    // The inbox shows it to Olga only as a look, never a release, and does
    // not count it as waiting on her.
    let inbox = commit(&db.state, &olga, "inbox.get", json!({}))
        .await
        .unwrap();
    let entry = inbox["result"]["bills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["bill"]["id"] == b)
        .expect("the bill is still visible to the overrider in the inbox");
    assert_eq!(entry["action"], "look");
    db.drop().await;
}
