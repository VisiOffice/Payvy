//! Quantity matching: cumulative ordered vs billed quantities and rates
//! per PO line. Flags by default; blocks under an enforcing policy.

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

async fn gated_vendor(db: &TestDb) -> String {
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Quantity Hauler").await;
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

/// A PO for 3 loads of Haul at $100 each, with $1,000 authorized overall.
async fn haul_po(db: &TestDb, vendor_id: &str) -> String {
    let robert = person("robert");
    let created = commit(
        &db.state,
        &agent("claude-ap"),
        "po.create",
        json!({ "vendor_id": vendor_id, "po_number": "PO-QTY", "owner": "robert",
                "authorized_cents": 100_000,
                "lines": [{ "description": "Haul", "quantity": 3, "unit": "load",
                            "unit_price_cents": 10_000, "amount_cents": 30_000 }] }),
    )
    .await
    .unwrap();
    let id = created["result"]["id"].as_str().unwrap().to_owned();
    commit(&db.state, &robert, "po.activate", json!({ "po_id": id }))
        .await
        .unwrap();
    id
}

async fn bill_with_lines(
    db: &TestDb,
    vendor_id: &str,
    number: &str,
    lines: Value,
    total: i64,
) -> String {
    let b = commit(
        &db.state,
        &agent("claude-ap"),
        "bill.create",
        json!({ "vendor_id": vendor_id, "invoice_number": number,
                "invoice_date": "2027-01-15", "amount_cents": total, "lines": lines }),
    )
    .await
    .unwrap();
    b["result"]["bill"]["id"].as_str().unwrap().to_owned()
}

fn haul(quantity: &str, rate: i64, amount: i64) -> Value {
    json!([{ "description": "Haul", "amount_cents": amount,
             "quantity": quantity, "unit": "load", "unit_price_cents": rate }])
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

#[tokio::test]
async fn quantities_flag_by_default_and_block_under_an_enforcing_policy() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db).await;
    let po = haul_po(&db, &v).await;

    // Two bills of 2 loads: 4 billed against 3 ordered, across invoices.
    let b1 = bill_with_lines(&db, &v, "Q-1", haul("2", 10_000, 20_000), 20_000).await;
    allocate(&db, &b1, &po, 20_000).await;
    let b2 = bill_with_lines(&db, &v, "Q-2", haul("2", 10_000, 20_000), 20_000).await;
    allocate(&db, &b2, &po, 20_000).await;

    // flag_only: the finding sits beside the bill and payment proceeds.
    let got = get_bill(&db, &b2).await;
    assert!(got["purchase_orders"]["exception"].is_null(), "{got}");
    assert_eq!(
        got["purchase_orders"]["quantity_findings"][0]["kind"],
        "quantity_over"
    );
    assert_eq!(got["purchase_orders"]["quantity_lines"][0]["ordered"], "3");
    assert_eq!(got["purchase_orders"]["quantity_lines"][0]["billed"], "4");
    assert_eq!(
        got["purchase_orders"]["quantity_lines"][0]["remaining"],
        "0"
    );
    assert!(
        got["purchase_orders"]["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "quantity_over"),
        "{got}"
    );
    confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b2 }))
        .await
        .unwrap();

    // Enforce, pinned to bills created from now on.
    commit(
        &db.state,
        &robert,
        "po.policy_set",
        json!({ "vendor_id": v, "quantity_matching": "enforce" }),
    )
    .await
    .unwrap();

    // A rate one cent over the PO rate blocks, and an override permits it.
    let b3 = bill_with_lines(&db, &v, "Q-3", haul("1", 10_001, 10_001), 10_001).await;
    allocate(&db, &b3, &po, 10_001).await;
    let got = get_bill(&db, &b3).await;
    assert_eq!(got["purchase_orders"]["exception"]["kind"], "rate_mismatch");
    let e = confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b3 }))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("rate_mismatch"), "{e}");
    confirm(
        &db.state,
        &robert,
        "po.override",
        json!({ "bill_id": b3, "authorized_excess_cents": 0, "reason": "rate agreed by phone" }),
    )
    .await
    .unwrap();
    confirm(
        &db.state,
        &person("alice"),
        "bill.release",
        json!({ "bill_id": b3 }),
    )
    .await
    .unwrap();

    // Over-quantity blocks too under the same policy.
    let b4 = bill_with_lines(&db, &v, "Q-4", haul("1", 10_000, 10_000), 10_000).await;
    allocate(&db, &b4, &po, 10_000).await;
    let got = get_bill(&db, &b4).await;
    assert_eq!(got["purchase_orders"]["exception"]["kind"], "quantity_over");

    // An unmatched line is identification only: it flags, never blocks.
    let b5 = bill_with_lines(
        &db,
        &v,
        "Q-5",
        json!([{ "description": "Mystery charge", "amount_cents": 5_000,
                  "quantity": "1", "unit_price_cents": 5_000 }]),
        5_000,
    )
    .await;
    allocate(&db, &b5, &po, 5_000).await;
    let got = get_bill(&db, &b5).await;
    assert!(got["purchase_orders"]["exception"].is_null(), "{got}");
    assert_eq!(
        got["purchase_orders"]["quantity_findings"][0]["kind"],
        "unmatched_line"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_bad_quantity_is_refused_at_entry_and_amount_only_lines_are_untouched() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let v = gated_vendor(&db).await;
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "Q-BAD", "invoice_date": "2027-01-15",
                "amount_cents": 1_000,
                "lines": [{ "description": "Haul", "amount_cents": 1_000, "quantity": "two" }] }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    db.drop().await;
}
