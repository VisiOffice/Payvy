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
    // Its own PO line, so quantity (already over on Haul) can't interfere.
    let created = commit(
        &db.state,
        &agent("claude-ap"),
        "po.create",
        json!({ "vendor_id": v, "po_number": "PO-RATE", "owner": "robert",
                "authorized_cents": 100_000,
                "lines": [{ "description": "Disposal", "quantity": 10, "unit": "load",
                            "unit_price_cents": 5_000, "amount_cents": 50_000 }] }),
    )
    .await
    .unwrap();
    let po_rate = created["result"]["id"].as_str().unwrap().to_owned();
    commit(
        &db.state,
        &robert,
        "po.activate",
        json!({ "po_id": po_rate }),
    )
    .await
    .unwrap();
    let b3 = bill_with_lines(
        &db,
        &v,
        "Q-3",
        json!([{ "description": "Disposal", "amount_cents": 5_001,
                  "quantity": "1", "unit": "load", "unit_price_cents": 5_001 }]),
        5_001,
    )
    .await;
    allocate(&db, &b3, &po_rate, 5_001).await;
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

#[tokio::test]
async fn prior_means_allocated_before_and_a_bill_counts_once_however_many_allocations() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let v = gated_vendor(&db).await;
    let po = haul_po(&db, &v).await;

    // Bill A allocates twice (10k + 10k) for its 2 loads; bill B follows.
    let a = bill_with_lines(&db, &v, "D-1", haul("2", 10_000, 20_000), 20_000).await;
    allocate(&db, &a, &po, 10_000).await;
    allocate(&db, &a, &po, 10_000).await;
    let b = bill_with_lines(&db, &v, "D-2", haul("2", 10_000, 20_000), 20_000).await;
    allocate(&db, &b, &po, 20_000).await;

    // A counts once (2, not 4), and B, allocated later, is not A's prior.
    let got = get_bill(&db, &a).await;
    assert_eq!(got["purchase_orders"]["quantity_lines"][0]["billed"], "2");
    assert!(
        got["purchase_orders"]["quantity_findings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{got}"
    );
    // B sees A before it: 2 + 2 over the ordered 3.
    let got = get_bill(&db, &b).await;
    assert_eq!(
        got["purchase_orders"]["quantity_findings"][0]["kind"],
        "quantity_over"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_unit_that_disagrees_never_matches_and_a_split_bill_counts_its_share_per_po() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db).await;
    let po = haul_po(&db, &v).await;

    // Same description, wrong unit: an identification finding, no match.
    let wrong = bill_with_lines(
        &db,
        &v,
        "U-1",
        json!([{ "description": "Haul", "amount_cents": 10_000,
                  "quantity": "1", "unit": "each", "unit_price_cents": 10_000 }]),
        10_000,
    )
    .await;
    allocate(&db, &wrong, &po, 10_000).await;
    let got = get_bill(&db, &wrong).await;
    assert_eq!(
        got["purchase_orders"]["quantity_findings"][0]["kind"],
        "unmatched_line"
    );

    // A second PO for the same work; one bill of 4 loads split across both
    // counts 2 against each, not 4.
    let created = commit(
        &db.state,
        &agent("claude-ap"),
        "po.create",
        json!({ "vendor_id": v, "po_number": "PO-QTY-2", "owner": "robert",
                "authorized_cents": 100_000,
                "lines": [{ "description": "Haul", "quantity": 3, "unit": "load",
                            "unit_price_cents": 10_000, "amount_cents": 30_000 }] }),
    )
    .await
    .unwrap();
    let po2 = created["result"]["id"].as_str().unwrap().to_owned();
    commit(&db.state, &robert, "po.activate", json!({ "po_id": po2 }))
        .await
        .unwrap();
    let split = bill_with_lines(&db, &v, "U-2", haul("4", 10_000, 40_000), 40_000).await;
    confirm(
        &db.state,
        &robert,
        "po.match_apply",
        json!({ "bill_id": split, "allocations": [
            { "po_id": po, "amount_cents": 20_000 },
            { "po_id": po2, "amount_cents": 20_000 }] }),
    )
    .await
    .unwrap();
    let got = get_bill(&db, &split).await;
    assert!(
        got["purchase_orders"]["quantity_findings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{got}"
    );
    let rows = got["purchase_orders"]["quantity_lines"].as_array().unwrap();
    assert!(rows.iter().all(|r| r["billed"] == "2"), "{rows:?}");
    db.drop().await;
}

#[tokio::test]
async fn an_override_binds_to_the_finding_it_permitted_and_quantity_never_hides_funding() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db).await;
    commit(
        &db.state,
        &robert,
        "po.policy_set",
        json!({ "vendor_id": v, "quantity_matching": "enforce" }),
    )
    .await
    .unwrap();
    let po = haul_po(&db, &v).await;

    // A rate-mismatch override permits that finding…
    let b = bill_with_lines(&db, &v, "S-1", haul("1", 10_001, 10_001), 10_001).await;
    allocate(&db, &b, &po, 10_001).await;
    confirm(
        &db.state,
        &robert,
        "po.override",
        json!({ "bill_id": b, "authorized_excess_cents": 0, "reason": "rate agreed" }),
    )
    .await
    .unwrap();
    let got = get_bill(&db, &b).await;
    assert_eq!(got["purchase_orders"]["overridden"], true, "{got}");

    // …but when the vendor sends a revised copy at yet another rate and a
    // person accepts it, the finding is a different one and the override
    // no longer counts.
    let rev = commit(
        &db.state,
        &agent("claude-ap"),
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "S-1", "invoice_date": "2027-01-15",
                "amount_cents": 10_002, "lines": haul("1", 10_002, 10_002) }),
    )
    .await
    .unwrap();
    assert_eq!(rev["result"]["arrival"]["kind"], "revision");
    commit(
        &db.state,
        &robert,
        "bill.revision_accept",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap();
    allocate(&db, &b, &po, 1).await;
    let got = get_bill(&db, &b).await;
    assert_eq!(got["purchase_orders"]["exception"]["kind"], "rate_mismatch");
    assert_eq!(got["purchase_orders"]["overridden"], false, "{got}");
    let e = confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("rate_mismatch"), "{e}");

    // And a bill failing both funding and quantity reports funding first.
    commit(
        &db.state,
        &robert,
        "po.set_vendor_settings",
        json!({ "vendor_id": v, "require_po": true, "require_funding": true }),
    )
    .await
    .unwrap();
    let b2 = bill_with_lines(&db, &v, "S-2", haul("8", 10_000, 80_000), 80_000).await;
    allocate(&db, &b2, &po, 80_000).await;
    let got = get_bill(&db, &b2).await;
    assert_eq!(
        got["purchase_orders"]["exception"]["kind"], "funding_not_verified",
        "{got}"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_tiny_allocation_does_not_hide_overbilling() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let v = gated_vendor(&db).await;
    let po = haul_po(&db, &v).await;

    // 40 loads on the bill, $1 allocated to a 3-load PO: the share is this
    // PO's allocation over the bill's total allocations (all of it), so
    // the full 40 count — the unallocated remainder does not shrink it.
    let b = bill_with_lines(&db, &v, "T-1", haul("40", 10_000, 400_000), 400_000).await;
    allocate(&db, &b, &po, 100).await;
    let got = get_bill(&db, &b).await;
    assert_eq!(
        got["purchase_orders"]["quantity_findings"][0]["kind"], "quantity_over",
        "{got}"
    );
    assert_eq!(got["purchase_orders"]["quantity_lines"][0]["billed"], "40");
    db.drop().await;
}

#[tokio::test]
async fn an_override_for_one_exception_never_clears_another() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db).await;
    commit(
        &db.state,
        &robert,
        "po.policy_set",
        json!({ "vendor_id": v, "quantity_matching": "enforce" }),
    )
    .await
    .unwrap();
    let po = haul_po(&db, &v).await;

    // Partly covered and 5 loads against 3 ordered: two exceptions stand.
    let b = bill_with_lines(&db, &v, "B-1", haul("5", 10_000, 50_000), 50_000).await;
    allocate(&db, &b, &po, 20_000).await;
    let got = get_bill(&db, &b).await;
    let kinds: Vec<&str> = got["purchase_orders"]["exceptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["uncovered", "quantity_over"], "{got}");

    // Permitting the shortfall leaves the quantity exception standing.
    confirm(
        &db.state,
        &robert,
        "po.override",
        json!({ "bill_id": b, "authorized_excess_cents": 30_000, "reason": "shortfall agreed" }),
    )
    .await
    .unwrap();
    let e = confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("quantity_over"), "{e}");

    // Permitting that too (the override targets the first unpermitted
    // exception) makes the bill payable — but neither overrider may
    // release it; a third person does.
    confirm(
        &db.state,
        &person("alice"),
        "po.override",
        json!({ "bill_id": b, "authorized_excess_cents": 0, "reason": "extra loads agreed" }),
    )
    .await
    .unwrap();
    let got = get_bill(&db, &b).await;
    assert_eq!(got["purchase_orders"]["overridden"], true, "{got}");
    let kinds: Vec<(bool, Option<&str>)> = got["purchase_orders"]["exceptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["overridden"].as_bool().unwrap(),
                e["overridden_by"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [(true, Some("robert")), (true, Some("alice"))],
        "{got}"
    );
    for who in [&robert, &person("alice")] {
        let e = confirm(&db.state, who, "bill.release", json!({ "bill_id": b }))
            .await
            .unwrap_err();
        assert!(matches!(e, CommandError::Forbidden(_)), "{e}");
    }
    confirm(
        &db.state,
        &person("charlie"),
        "bill.release",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn a_revised_quantity_is_checked_once_accepted_not_before() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = gated_vendor(&db).await;
    let po = haul_po(&db, &v).await;
    // 2 of 3 loads: within the ordered quantity.
    let b = bill_with_lines(&db, &v, "RQ-9", haul("2", 10_000, 20_000), 20_000).await;
    allocate(&db, &b, &po, 20_000).await;
    let got = get_bill(&db, &b).await;
    assert!(
        got["purchase_orders"]["quantity_findings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{got}"
    );

    // The vendor resends the same total with 5 loads on it. Until a
    // person accepts the revision, the old lines are what is checked.
    let rev = commit(
        &db.state,
        &agent("claude-ap"),
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "RQ-9", "invoice_date": "2027-01-15",
                "amount_cents": 20_000, "lines": haul("5", 4_000, 20_000) }),
    )
    .await
    .unwrap();
    assert_eq!(rev["result"]["arrival"]["kind"], "revision");
    let got = get_bill(&db, &b).await;
    assert!(
        got["purchase_orders"]["quantity_findings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{got}"
    );

    commit(
        &db.state,
        &robert,
        "bill.revision_accept",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap();
    let got = get_bill(&db, &b).await;
    let kinds: Vec<&str> = got["purchase_orders"]["quantity_findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"quantity_over"), "{got}");
    db.drop().await;
}
