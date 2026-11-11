//! Bills moving through vendor workflows against a real database.

use payvy_server::commands::{self, CommandError, Mode};
use serde_json::json;

mod common;
use common::{TestDb, agent, bill, commit, confirm, person, set_workflow, status, vendor};

/// The Quarry shape: big bills need the controller, every bill waits for
/// the customer's payment, then a 3-day hold, then it pays automatically.
#[tokio::test]
async fn quarry_workflow_end_to_end() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let (robert, ap_agent) = (person("robert"), agent("claude-ap"));
    let hauler = vendor(s, &ap_agent, "King Environmental").await;
    set_workflow(
        s,
        &ap_agent,
        Some(&hauler),
        json!([
            {"id": "controller", "type": "approve", "approvers": ["robert"], "min_amount_cents": 500_000},
            {"id": "funded", "type": "wait_for", "signal": "customer_funded", "label": "the customer's payment"},
            {"id": "cooling", "type": "hold", "days": 3},
            {"id": "pay", "type": "pay", "release": "auto"}
        ]),
    )
    .await;

    // Small bill: skips approval, waits for funding (signalled by Quarry's
    // order number), holds 3 days, then is ready to pay.
    let small = commit(
        s,
        &ap_agent,
        "bill.create",
        json!({"vendor_id": hauler, "invoice_number": "189727", "invoice_date": "2027-01-15",
               "amount_cents": 42_500, "external_ref": "quarry-order-5512"}),
    )
    .await
    .unwrap();
    let small_id = small["result"]["bill"]["id"].as_str().unwrap().to_owned();
    assert_eq!(small["result"]["progress"]["status"], "waiting_for");
    assert_eq!(small["result"]["progress"]["steps"][0]["state"], "skipped");

    let sig = commit(
        s,
        &ap_agent,
        "bill.signal",
        json!({"external_ref": "quarry-order-5512", "signal": "customer_funded"}),
    )
    .await
    .unwrap();
    assert_eq!(sig["result"]["signal_used_by_step"], "funded");
    assert_eq!(sig["result"]["progress"]["status"], "held");
    db.advance(3);
    assert_eq!(status(s, &robert, &small_id).await, "ready_to_pay");

    // Big bill: the agent can't approve; a stranger can't either; robert can.
    let big = bill(s, &ap_agent, &hauler, "189800", 650_000).await;
    assert_eq!(status(s, &robert, &big).await, "awaiting_approval");
    let e = commit(s, &ap_agent, "bill.approve", json!({"bill_id": big}))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let e = commit(
        s,
        &person("mallory"),
        "bill.approve",
        json!({"bill_id": big}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Forbidden(_)), "{e}");
    let mine = commit(s, &robert, "bill.list", json!({"mine": true}))
        .await
        .unwrap();
    assert_eq!(mine["result"]["bills"].as_array().unwrap().len(), 1);
    commit(
        s,
        &robert,
        "bill.approve",
        json!({"bill_id": big, "note": "checked against the PO"}),
    )
    .await
    .unwrap();
    assert_eq!(status(s, &robert, &big).await, "waiting_for");

    // Paid outside Payvy, recorded with a confirmation.
    let paid = confirm(
        s,
        &ap_agent,
        "bill.mark_paid",
        json!({"bill_id": small_id, "paid_on": "2027-01-20", "method": "ach"}),
    )
    .await
    .unwrap();
    assert_eq!(paid["result"]["bill"]["state"], "paid");
    let history = paid["result"]["history"].as_array().unwrap();
    let kinds: Vec<&str> = history
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["bill.paid", "bill.signalled", "bill.created"]);
    db.drop().await;
}

#[tokio::test]
async fn release_needs_a_person_and_a_confirmation() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let v = vendor(s, &person("robert"), "Sun Pac").await;
    let b = bill(s, &agent("claude-ap"), &v, "A-1", 10_000).await;
    // Built-in default: any person releases.
    assert_eq!(status(s, &person("robert"), &b).await, "awaiting_release");

    let e = commit(
        s,
        &agent("claude-ap"),
        "bill.release",
        json!({"bill_id": b}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)));
    let e = commit(s, &person("robert"), "bill.release", json!({"bill_id": b}))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::NeedsConfirmation(_)));

    // A preview changes nothing.
    let preview = commands::run(
        s,
        &person("robert"),
        "bill.release",
        json!({"bill_id": b}),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(preview["result"]["progress"]["status"], "released");
    assert_eq!(status(s, &person("robert"), &b).await, "awaiting_release");

    // A token is bound to its input and used once.
    let token = preview["confirm_token"].as_str().unwrap().to_owned();
    let other = bill(s, &person("robert"), &v, "A-2", 99_999).await;
    let e = commands::run(
        s,
        &person("robert"),
        "bill.release",
        json!({"bill_id": other}),
        Mode::Commit {
            confirm_token: Some(token.clone()),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::BadToken(_)));
    commands::run(
        s,
        &person("robert"),
        "bill.release",
        json!({"bill_id": b}),
        Mode::Commit {
            confirm_token: Some(token.clone()),
        },
    )
    .await
    .unwrap();
    let e = commands::run(
        s,
        &person("robert"),
        "bill.release",
        json!({"bill_id": b}),
        Mode::Commit {
            confirm_token: Some(token),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::BadToken(_)));
    assert_eq!(status(s, &person("robert"), &b).await, "released");
    db.drop().await;
}

#[tokio::test]
async fn bills_keep_the_workflow_they_arrived_under() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let robert = person("robert");
    let v = vendor(s, &robert, "United Rentals").await;
    let before = bill(s, &robert, &v, "1", 1_000).await;
    set_workflow(
        s,
        &agent("claude-ap"),
        Some(&v),
        json!([
            {"id": "ok", "type": "approve", "approvers": ["ann"]},
            {"id": "pay", "type": "pay", "release": "auto"}
        ]),
    )
    .await;
    let after = bill(s, &robert, &v, "2", 1_000).await;
    assert_eq!(status(s, &robert, &before).await, "awaiting_release");
    assert_eq!(status(s, &robert, &after).await, "awaiting_approval");

    // An organization default applies to vendors without their own.
    let other = vendor(s, &robert, "National").await;
    set_workflow(s, &robert, None, json!([{"id": "h", "type": "hold", "days": 2}, {"id": "pay", "type": "pay", "release": "auto"}])).await;
    let b = bill(s, &robert, &other, "9", 1_000).await;
    assert_eq!(status(s, &robert, &b).await, "held");
    let wf = commit(s, &robert, "workflow.get", json!({"vendor_id": other}))
        .await
        .unwrap();
    assert_eq!(wf["result"]["source"], "organization");
    assert!(
        wf["result"]["workflow"]["mermaid"]
            .as_str()
            .unwrap()
            .contains("Hold 2 days")
    );
    db.drop().await;
}

#[tokio::test]
async fn proposals_wait_for_a_person() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let v = vendor(s, &person("robert"), "Zippy").await;
    let steps = json!({"steps": [{"id": "pay", "type": "pay", "release": "auto"}]});
    let p = commit(
        s,
        &agent("claude-ap"),
        "workflow.propose",
        json!({"vendor_id": v, "workflow": steps, "reason": "always correct"}),
    )
    .await
    .unwrap();
    let id = p["result"]["proposal_id"].as_str().unwrap().to_owned();
    let e = commit(
        s,
        &agent("claude-ap"),
        "workflow.accept",
        json!({"proposal_id": id}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)));
    let got = commit(s, &person("robert"), "vendor.get", json!({"vendor_id": v}))
        .await
        .unwrap();
    assert_eq!(got["result"]["workflow"]["source"], "builtin");
    assert_eq!(
        got["result"]["workflow"]["proposals"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let listed = commit(s, &person("robert"), "vendor.list", json!({}))
        .await
        .unwrap();
    let zippy = &listed["result"]["vendors"][0];
    assert_eq!(zippy["own_workflow"], false);
    assert_eq!(zippy["workflow_proposals"], 1);

    commit(
        s,
        &person("robert"),
        "workflow.decline",
        json!({"proposal_id": id, "reason": "not yet"}),
    )
    .await
    .unwrap();
    let listed = commit(s, &person("robert"), "vendor.list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["result"]["vendors"][0]["workflow_proposals"], 0);
    set_workflow(
        s,
        &agent("claude-ap"),
        Some(&v),
        json!([{"id": "pay", "type": "pay", "release": "manual", "by": ["robert"]}]),
    )
    .await;
    let listed = commit(s, &person("robert"), "vendor.list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["result"]["vendors"][0]["own_workflow"], true);
    let e = commit(
        s,
        &person("robert"),
        "workflow.accept",
        json!({"proposal_id": id}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)));

    let bad =
        json!({"steps": [{"id": "pay", "type": "pay"}, {"id": "h", "type": "hold", "days": 1}]});
    let e = commit(
        s,
        &agent("claude-ap"),
        "workflow.propose",
        json!({"workflow": bad}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn duplicates_are_caught_and_voiding_frees_the_number() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let robert = person("robert");
    let v = vendor(s, &robert, "American Reclamation").await;
    let first = bill(s, &robert, &v, "INV-00086641", 5_000).await;
    let input = json!({"vendor_id": v, "invoice_number": "inv 86641", "invoice_date": "2027-01-15", "amount_cents": 5_000});
    let e = commit(s, &robert, "bill.create", input.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains(&first)),
        "{e}"
    );

    confirm(
        s,
        &robert,
        "bill.void",
        json!({"bill_id": first, "reason": "entered twice"}),
    )
    .await
    .unwrap();
    commit(s, &robert, "bill.create", input).await.unwrap();

    // Lines must add up; due date comes from the vendor's terms.
    let e = commit(
        s,
        &robert,
        "bill.create",
        json!({"vendor_id": v, "invoice_number": "X", "invoice_date": "2027-01-15",
        "amount_cents": 100, "lines": [{"description": "a", "amount_cents": 60}]}),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)));
    let ok = commit(s, &robert, "bill.create", json!({"vendor_id": v, "invoice_number": "Y", "invoice_date": "2027-01-15", "amount_cents": 100}))
        .await
        .unwrap();
    assert_eq!(ok["result"]["bill"]["due_date"], "2027-02-14");
    db.drop().await;
}

#[tokio::test]
async fn organizations_cannot_see_each_other() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let v = vendor(s, &person("robert"), "Ours").await;
    let b = bill(s, &person("robert"), &v, "1", 100).await;
    let other = payvy_server::Actor::administrator(db.add_org().await, "eve");
    let e = commit(s, &other, "bill.get", json!({"bill_id": b}))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::NotFound(_)));
    let list = commit(s, &other, "vendor.list", json!({})).await.unwrap();
    assert_eq!(list["result"]["vendors"], json!([]));
    db.drop().await;
}

#[tokio::test]
async fn the_inbox_shows_what_waits_on_a_person() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let robert = person("robert");
    let v = vendor(s, &robert, "King Environmental").await;
    set_workflow(
        s,
        &robert,
        Some(&v),
        json!([
            { "id": "ok", "type": "approve", "approvers": ["robert"] },
            { "id": "pay", "type": "pay", "release": "manual" }
        ]),
    )
    .await;
    let b = common::bill(s, &agent("claude-ap"), &v, "189727", 45_900).await;
    commit(
        s,
        &agent("claude-ap"),
        "workflow.propose",
        json!({ "workflow": { "steps": [{ "id": "pay", "type": "pay", "release": "auto" }] },
                "reason": "small vendors" }),
    )
    .await
    .unwrap();

    let inbox = commit(s, &robert, "inbox.get", json!({})).await.unwrap()["result"].clone();
    assert_eq!(inbox["count"], 2, "{inbox:#}");
    assert_eq!(inbox["bills"][0]["bill"]["id"], b.as_str());
    assert_eq!(inbox["bills"][0]["action"], "approve");
    assert!(inbox["workflow_proposals"][0]["vendor_name"].is_null());
    assert_eq!(
        inbox["workflow_proposals"][0]["proposal"]["reason"],
        "small vendors"
    );

    // Someone else isn't asked to approve robert's bill.
    let other = commit(s, &person("maria"), "inbox.get", json!({}))
        .await
        .unwrap()["result"]
        .clone();
    assert_eq!(other["bills"].as_array().unwrap().len(), 0);

    commit(s, &robert, "bill.approve", json!({ "bill_id": b }))
        .await
        .unwrap();
    let inbox = commit(s, &robert, "inbox.get", json!({})).await.unwrap()["result"].clone();
    assert_eq!(inbox["bills"][0]["action"], "release");
    db.drop().await;
}
