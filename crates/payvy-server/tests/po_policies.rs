//! The enumerated policy registry and the suspended lifecycle: policies
//! are versioned and pinned to bills like workflows; a suspended PO's
//! allocations stand but its bills wait.

use payvy_server::commands::CommandError;
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, commit, confirm, person, vendor};

async fn po_on(db: &TestDb) {
    commit(
        &db.state,
        &person("admin"),
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": "robert@example.com", "name": "Robert", "actor": "robert" }),
    )
    .await
    .unwrap();
}

/// A vendor whose bills require a PO and go straight to a manual pay step.
async fn gated_vendor(db: &TestDb, name: &str) -> String {
    let robert = person("robert");
    let v = vendor(&db.state, &robert, name).await;
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
        json!({ "vendor_id": v, "require_po": true }),
    )
    .await
    .unwrap();
    v
}

async fn active_po(
    db: &TestDb,
    vendor_id: &str,
    number: &str,
    cents: i64,
    period: Option<(&str, &str)>,
) -> String {
    let robert = person("robert");
    let mut input = json!({ "vendor_id": vendor_id, "po_number": number,
        "owner": "robert", "authorized_cents": cents });
    if let Some((start, end)) = period {
        input["period_start"] = json!(start);
        input["period_end"] = json!(end);
    }
    let created = commit(&db.state, &agent("claude-ap"), "po.create", input)
        .await
        .unwrap();
    let id = created["result"]["id"].as_str().unwrap().to_owned();
    commit(&db.state, &robert, "po.activate", json!({ "po_id": id }))
        .await
        .unwrap();
    id
}

async fn make_bill(db: &TestDb, vendor_id: &str, number: &str, cents: i64, date: &str) -> String {
    let b = commit(
        &db.state,
        &agent("claude-ap"),
        "bill.create",
        json!({ "vendor_id": vendor_id, "invoice_number": number,
                "invoice_date": date, "amount_cents": cents }),
    )
    .await
    .unwrap();
    b["result"]["bill"]["id"].as_str().unwrap().to_owned()
}

async fn get_bill(db: &TestDb, bill_id: &str) -> Value {
    commit(
        &db.state,
        &person("robert"),
        "bill.get",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap()["result"]
        .clone()
}

async fn allocate(db: &TestDb, bill_id: &str, po_id: &str, cents: i64) {
    confirm(
        &db.state,
        &person("robert"),
        "po.match_apply",
        json!({ "bill_id": bill_id, "allocations": [{ "po_id": po_id, "amount_cents": cents }] }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn over_period_flags_by_default_blocks_when_the_pinned_policy_requires_cover() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db, "Allied").await;
    // A January PO; the invoice is dated March (Allied's Apple Valley shape).
    let po = active_po(
        &db,
        &v,
        "PO-PERIOD",
        100_000,
        Some(("2027-01-01", "2027-01-31")),
    )
    .await;

    let defaults = commit(
        &db.state,
        &robert,
        "po.policy_get",
        json!({ "vendor_id": v }),
    )
    .await
    .unwrap();
    assert_eq!(defaults["result"]["version"], 0);
    assert_eq!(defaults["result"]["over_period"], "flag_only");

    // flag_only: fully allocated, flagged beside the bill, pays normally.
    let b1 = make_bill(&db, &v, "P-1", 40_000, "2027-03-15").await;
    allocate(&db, &b1, &po, 40_000).await;
    let got = get_bill(&db, &b1).await;
    assert!(got["purchase_orders"]["exception"].is_null(), "{got}");
    assert_eq!(got["purchase_orders"]["notices"][0]["kind"], "over_period");
    assert_eq!(got["purchase_orders"]["policy"]["over_period"], "flag_only");
    confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b1 }))
        .await
        .unwrap();

    // Only a person sets policy; a change is a new version; a no-op refuses.
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "po.policy_set",
        json!({ "vendor_id": v, "over_period": "require_full_cover" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let set = commit(
        &db.state,
        &robert,
        "po.policy_set",
        json!({ "vendor_id": v, "over_period": "require_full_cover", "reason": "Apple Valley" }),
    )
    .await
    .unwrap();
    assert_eq!(set["result"]["version"], 1);
    let e = commit(
        &db.state,
        &robert,
        "po.policy_set",
        json!({ "vendor_id": v, "over_period": "require_full_cover" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    // A new bill pins version 1: the same shape now blocks payment.
    let b2 = make_bill(&db, &v, "P-2", 40_000, "2027-03-15").await;
    allocate(&db, &b2, &po, 40_000).await;
    let got = get_bill(&db, &b2).await;
    assert_eq!(got["purchase_orders"]["exception"]["kind"], "over_period");
    assert_eq!(got["purchase_orders"]["policy"]["version"], 1);
    let e = confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b2 }))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("over_period"), "{e}");

    // Setting it back creates version 2; b2 keeps the version it pinned.
    commit(
        &db.state,
        &robert,
        "po.policy_set",
        json!({ "vendor_id": v, "over_period": "flag_only" }),
    )
    .await
    .unwrap();
    let got = get_bill(&db, &b2).await;
    assert_eq!(got["purchase_orders"]["exception"]["kind"], "over_period");
    let b3 = make_bill(&db, &v, "P-3", 20_000, "2027-03-15").await;
    allocate(&db, &b3, &po, 20_000).await;
    let got = get_bill(&db, &b3).await;
    assert!(got["purchase_orders"]["exception"].is_null(), "{got}");
    assert_eq!(got["purchase_orders"]["policy"]["version"], 2);
    db.drop().await;
}

#[tokio::test]
async fn an_override_survives_the_shortfall_flipping_between_uncovered_and_over_available() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db, "Flip Hauler").await;
    let small = active_po(&db, &v, "PO-A", 30_000, None).await;
    let big = active_po(&db, &v, "PO-B", 100_000, None).await;
    let b = make_bill(&db, &v, "F-1", 50_000, "2027-01-15").await;
    allocate(&db, &b, &small, 30_000).await;

    // Room remains on PO-B, so the $200 shortfall is "uncovered" for now.
    let got = get_bill(&db, &b).await;
    assert_eq!(got["purchase_orders"]["exception"]["kind"], "uncovered");
    confirm(
        &db.state,
        &robert,
        "po.override",
        json!({ "bill_id": b, "authorized_excess_cents": 20_000, "reason": "short haul, agreed" }),
    )
    .await
    .unwrap();

    // Another bill uses up PO-B's room. Nothing about b changed, but its
    // shortfall is now "over_available" — the override must still count.
    let other = make_bill(&db, &v, "F-2", 100_000, "2027-01-15").await;
    allocate(&db, &other, &big, 100_000).await;
    let got = get_bill(&db, &b).await;
    assert_eq!(
        got["purchase_orders"]["exception"]["kind"],
        "over_available"
    );
    assert_eq!(got["purchase_orders"]["overridden"], true, "{got}");
    confirm(
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
async fn a_suspended_po_keeps_its_allocations_and_its_bills_wait() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db, "Whittier").await;
    let po = active_po(&db, &v, "PO-SUSP", 100_000, None).await;
    let b = make_bill(&db, &v, "S-1", 50_000, "2027-01-15").await;
    allocate(&db, &b, &po, 50_000).await;

    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "po.suspend",
        json!({ "po_id": po, "reason": "order suspended by the customer" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let suspended = commit(
        &db.state,
        &robert,
        "po.suspend",
        json!({ "po_id": po, "reason": "order suspended by the customer" }),
    )
    .await
    .unwrap();
    assert_eq!(suspended["result"]["status"], "suspended");
    assert_eq!(suspended["result"]["amounts"]["reserved_cents"], 50_000);

    // No new allocations while suspended, and the allocated bill waits.
    let b2 = make_bill(&db, &v, "S-2", 10_000, "2027-01-15").await;
    let e = confirm(
        &db.state,
        &robert,
        "po.match_apply",
        json!({ "bill_id": b2, "allocations": [{ "po_id": po, "amount_cents": 10_000 }] }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("suspended"), "{e}");
    let got = get_bill(&db, &b).await;
    assert_eq!(got["purchase_orders"]["exception"]["kind"], "suspended_po");
    let e = confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("suspended_po"), "{e}");

    // Reactivation restores the bill; the allocation was never released.
    let back = commit(&db.state, &robert, "po.reactivate", json!({ "po_id": po }))
        .await
        .unwrap();
    assert_eq!(back["result"]["status"], "active");
    let got = get_bill(&db, &b).await;
    assert!(got["purchase_orders"]["exception"].is_null(), "{got}");
    confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn over_available_is_a_standing_exception_when_no_po_has_room_for_the_rest() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db, "Gated Hauler").await;
    let po = active_po(&db, &v, "PO-SMALL", 10_000, None).await;
    let b = make_bill(&db, &v, "OA-1", 50_000, "2027-01-15").await;
    allocate(&db, &b, &po, 10_000).await;

    let got = get_bill(&db, &b).await;
    assert_eq!(
        got["purchase_orders"]["exception"]["kind"],
        "over_available"
    );
    assert_eq!(got["purchase_orders"]["exception"]["excess_cents"], 40_000);

    // A scoped override permits exactly that excess; someone else releases.
    confirm(
        &db.state,
        &robert,
        "po.override",
        json!({ "bill_id": b, "authorized_excess_cents": 40_000, "reason": "one-off haul" }),
    )
    .await
    .unwrap();
    confirm(
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
async fn permitting_a_suspended_po_never_skips_the_other_exceptions() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db, "Partial Hauler").await;
    let po = active_po(&db, &v, "PO-PART", 100_000, None).await;
    let b = make_bill(&db, &v, "P-1", 50_000, "2027-01-15").await;
    // $300 of the bill is not covered by a PO.
    allocate(&db, &b, &po, 20_000).await;
    commit(
        &db.state,
        &robert,
        "po.suspend",
        json!({ "po_id": po, "reason": "on hold" }),
    )
    .await
    .unwrap();
    let got = get_bill(&db, &b).await;
    assert_eq!(got["purchase_orders"]["exception"]["kind"], "suspended_po");

    // Permitting the suspension is one override; the shortfall still stands.
    confirm(
        &db.state,
        &robert,
        "po.override",
        json!({ "bill_id": b, "authorized_excess_cents": 20_000, "reason": "pay despite hold" }),
    )
    .await
    .unwrap();
    let got = get_bill(&db, &b).await;
    assert_eq!(got["purchase_orders"]["overridden"], false, "{got}");
    let e = confirm(
        &db.state,
        &person("alice"),
        "bill.release",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("not covered by a PO"), "{e}");
    db.drop().await;
}
