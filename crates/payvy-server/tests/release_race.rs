//! The payment gate serializes against changes that would alter its
//! answer: while a release holds the bill's PO and allocation locks, a
//! suspend or a reversal waits — and the gate then sees their result.

use std::time::Duration;

use payvy_server::commands::CommandError;
use serde_json::json;
use uuid::Uuid;

mod common;
use common::{TestDb, agent, commit, confirm, person, vendor};

async fn setup(db: &TestDb) -> (String, String, String) {
    let robert = person("robert");
    commit(
        &db.state,
        &robert,
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &robert,
        "person.add",
        json!({ "email": "robert@example.com", "name": "Robert", "actor": "robert" }),
    )
    .await
    .unwrap();
    let v = vendor(&db.state, &robert, "Race Hauler").await;
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
    let created = commit(
        &db.state,
        &agent("claude-ap"),
        "po.create",
        json!({ "vendor_id": v, "po_number": "PO-RACE", "owner": "robert", "authorized_cents": 100_000 }),
    )
    .await
    .unwrap();
    let po = created["result"]["id"].as_str().unwrap().to_owned();
    commit(&db.state, &robert, "po.activate", json!({ "po_id": po }))
        .await
        .unwrap();
    let b = commit(
        &db.state,
        &agent("claude-ap"),
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "RACE-1", "invoice_date": "2027-01-15", "amount_cents": 50_000 }),
    )
    .await
    .unwrap()["result"]["bill"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    confirm(
        &db.state,
        &robert,
        "po.match_apply",
        json!({ "bill_id": b, "allocations": [{ "po_id": po, "amount_cents": 50_000 }] }),
    )
    .await
    .unwrap();
    (v, po, b)
}

#[tokio::test]
async fn a_release_waits_for_the_po_lock_and_then_sees_the_suspension() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (_v, po, b) = setup(&db).await;
    let robert = person("robert");
    let po_id = Uuid::parse_str(&po).unwrap();

    // Hold the PO row the way a suspend in flight would need it.
    let mut hold = db
        .state
        .store
        .begin(payvy_store::DEFAULT_ORG)
        .await
        .unwrap();
    payvy_store::purchase_orders::lock_pos(&mut hold, &[po_id])
        .await
        .unwrap();

    let state = db.state.clone();
    let bill_id = b.clone();
    let mut release = tokio::spawn(async move {
        confirm(
            &state,
            &person("robert"),
            "bill.release",
            json!({ "bill_id": bill_id }),
        )
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(400), &mut release)
            .await
            .is_err(),
        "the release must wait while the PO is locked"
    );

    // The held transaction itself suspends and commits — exactly the
    // race: its UPDATE waits behind nothing, the release waits behind it,
    // and then sees the suspension.
    payvy_store::purchase_orders::set_status(
        &mut hold,
        po_id,
        "suspended",
        None,
        Some("customer suspended the order"),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    hold.commit().await.unwrap();
    let released = release.await.unwrap();
    let e = released.unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("suspended_po"), "{e}");
    let got = commit(&db.state, &robert, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(got["result"]["progress"]["status"], "awaiting_release");
    db.drop().await;
}

#[tokio::test]
async fn a_release_waits_for_the_allocation_lock_and_then_sees_the_reversal() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (_v, _po, b) = setup(&db).await;
    let robert = person("robert");
    let bill_id = Uuid::parse_str(&b).unwrap();
    let got = commit(&db.state, &robert, "bill.get", json!({ "bill_id": b }))
        .await
        .unwrap();
    let allocation = got["result"]["purchase_orders"]["allocations"][0]["allocation_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut hold = db
        .state
        .store
        .begin(payvy_store::DEFAULT_ORG)
        .await
        .unwrap();
    payvy_store::purchase_orders::lock_allocations_for_bill(&mut hold, bill_id)
        .await
        .unwrap();

    let state = db.state.clone();
    let bill_for_release = b.clone();
    let mut release = tokio::spawn(async move {
        confirm(
            &state,
            &person("robert"),
            "bill.release",
            json!({ "bill_id": bill_for_release }),
        )
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(400), &mut release)
            .await
            .is_err(),
        "the release must wait while its allocations are locked"
    );

    drop(hold);
    commit(
        &db.state,
        &robert,
        "po.allocation_reverse",
        json!({ "allocation_id": allocation, "reason": "wrong PO" }),
    )
    .await
    .unwrap();
    let e = release.await.unwrap().unwrap_err();
    assert!(e.to_string().contains("no_po"), "{e}");
    db.drop().await;
}
