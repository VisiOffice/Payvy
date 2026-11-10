//! Separation of duties on bill approvals, against a real database: the
//! company rule that whoever entered a bill can't approve it, and one
//! approval per person per bill.

use base64::Engine;
use payvy_server::commands::CommandError;
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, bill, commit, person, set_workflow, status, vendor};

async fn add_person(db: &TestDb, actor: &str) {
    commit(
        &db.state,
        &person("robert"),
        "person.add",
        json!({ "email": format!("{actor}@example.com"), "name": actor, "actor": actor }),
    )
    .await
    .unwrap();
}

async fn get(db: &TestDb, who: &payvy_server::Actor, bill_id: &str) -> Value {
    commit(&db.state, who, "bill.get", json!({ "bill_id": bill_id }))
        .await
        .unwrap()["result"]
        .clone()
}

async fn approve(
    db: &TestDb,
    who: &payvy_server::Actor,
    bill_id: &str,
) -> Result<Value, CommandError> {
    commit(
        &db.state,
        who,
        "bill.approve",
        json!({ "bill_id": bill_id }),
    )
    .await
}

async fn set_rule(db: &TestDb, on: bool) -> Result<Value, CommandError> {
    commit(
        &db.state,
        &person("robert"),
        "approval_rules.set",
        json!({ "submitter_cannot_approve": on, "reason": "auditor asked" }),
    )
    .await
}

#[tokio::test]
async fn the_rule_is_off_by_default_and_only_a_person_changes_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let rules = commit(s, &agent("claude-ap"), "approval_rules.get", json!({}))
        .await
        .unwrap();
    assert_eq!(rules["result"]["submitter_cannot_approve"], false);
    assert!(rules["result"]["updated_by"].is_null());

    let e = commit(
        s,
        &agent("claude-ap"),
        "approval_rules.set",
        json!({ "submitter_cannot_approve": true }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    let on = set_rule(&db, true).await.unwrap();
    assert_eq!(on["result"]["submitter_cannot_approve"], true);
    assert_eq!(on["result"]["updated_by"], "robert");
    assert_eq!(on["events"], json!(["approval_rules.changed"]));
    // A change that changes nothing is an error.
    let e = set_rule(&db, true).await.unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    let log = commit(s, &agent("claude-ap"), "event.list", json!({}))
        .await
        .unwrap();
    let changed = log["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "approval_rules.changed")
        .expect("the change is in the event log")
        .clone();
    assert_eq!(changed["actor"], "robert");
    assert_eq!(changed["payload"]["submitter_cannot_approve"], true);
    assert_eq!(changed["payload"]["was"], false);
    assert_eq!(changed["payload"]["reason"], "auditor asked");

    // Another organization's rules are its own.
    let other = db.add_org().await;
    let theirs = commit(
        s,
        &payvy_server::Actor::person(other, "robert"),
        "approval_rules.get",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(theirs["result"]["submitter_cannot_approve"], false);
    db.drop().await;
}

#[tokio::test]
async fn whoever_entered_a_bill_cant_approve_it_when_the_rule_is_on() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let (robert, maria) = (person("robert"), person("maria"));
    add_person(&db, "robert").await;
    add_person(&db, "maria").await;
    let v = vendor(s, &robert, "Copper State Roofing").await;
    set_workflow(
        s,
        &robert,
        Some(&v),
        json!([
            { "id": "ok", "type": "approve" },
            { "id": "pay", "type": "pay", "release": "manual" }
        ]),
    )
    .await;

    // Off: robert may approve the bill he entered, as before.
    let own = bill(s, &robert, &v, "CS-1", 98_000).await;
    assert_eq!(
        get(&db, &robert, &own).await["approval"]["can_approve"],
        true
    );

    set_rule(&db, true).await.unwrap();
    let detail = get(&db, &robert, &own).await;
    assert_eq!(detail["approval"]["can_approve"], false, "{detail:#}");
    assert_eq!(detail["approval"]["refusal"]["code"], "entered");
    assert_eq!(detail["approval"]["others"], json!(["maria"]));

    let e = approve(&db, &robert, &own).await.unwrap_err();
    let CommandError::Forbidden(msg) = &e else {
        panic!("{e:?}")
    };
    assert!(msg.contains("you entered this bill"), "{msg}");
    assert!(msg.contains("such as maria"), "{msg}");
    assert_eq!(status(s, &robert, &own).await, "awaiting_approval");

    // robert's inbox shows it waiting on someone else, not counted as his;
    // bill.list mine leaves it out; maria's inbox asks her.
    let inbox = commit(s, &robert, "inbox.get", json!({})).await.unwrap()["result"].clone();
    assert_eq!(inbox["count"], 0, "{inbox:#}");
    assert_eq!(inbox["bills"][0]["action"], "look");
    assert_eq!(inbox["bills"][0]["approval"]["others"], json!(["maria"]));
    let mine = commit(s, &robert, "bill.list", json!({ "mine": true }))
        .await
        .unwrap();
    assert_eq!(mine["result"]["bills"], json!([]));
    let hers = commit(s, &maria, "inbox.get", json!({})).await.unwrap()["result"].clone();
    assert_eq!(hers["bills"][0]["action"], "approve");
    approve(&db, &maria, &own).await.unwrap();
    assert_eq!(status(s, &robert, &own).await, "awaiting_release");

    // An agent isn't a person: its bills are approved by anyone, as before.
    let agents = bill(s, &agent("claude-ap"), &v, "CS-2", 12_000).await;
    approve(&db, &robert, &agents).await.unwrap();

    // Off again: the rule applies to bills already waiting, both ways.
    let later = bill(s, &robert, &v, "CS-3", 5_000).await;
    approve(&db, &robert, &later).await.unwrap_err();
    set_rule(&db, false).await.unwrap();
    approve(&db, &robert, &later).await.unwrap();
    db.drop().await;
}

#[tokio::test]
async fn one_person_gives_a_bill_one_approval() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let (robert, maria) = (person("robert"), person("maria"));
    let ap = agent("claude-ap");
    let v = vendor(s, &ap, "King Environmental").await;
    set_workflow(
        s,
        &ap,
        Some(&v),
        json!([
            { "id": "first", "type": "approve" },
            { "id": "second", "type": "approve" },
            { "id": "pay", "type": "pay", "release": "manual" }
        ]),
    )
    .await;
    let b = bill(s, &ap, &v, "189727", 45_900).await;
    approve(&db, &robert, &b).await.unwrap();
    let e = approve(&db, &robert, &b).await.unwrap_err();
    let CommandError::Forbidden(msg) = &e else {
        panic!("{e:?}")
    };
    assert!(
        msg.contains("you already approved this bill at step first"),
        "{msg}"
    );
    assert!(msg.contains("Any other person can approve it"), "{msg}");
    assert_eq!(status(s, &robert, &b).await, "awaiting_approval");
    approve(&db, &maria, &b).await.unwrap();
    assert_eq!(status(s, &robert, &b).await, "awaiting_release");

    // Approving an open step must not use up the only person a later step
    // names; the refusal says who else can take this one.
    set_workflow(
        s,
        &ap,
        Some(&v),
        json!([
            { "id": "anyone", "type": "approve" },
            { "id": "controller", "type": "approve", "approvers": ["robert"], "min_amount_cents": 100_000 },
            { "id": "pay", "type": "pay", "release": "manual" }
        ]),
    )
    .await;
    let big = bill(s, &ap, &v, "189800", 650_000).await;
    let e = approve(&db, &robert, &big).await.unwrap_err();
    assert!(
        e.to_string().contains("step controller needs you later"),
        "{e}"
    );
    approve(&db, &maria, &big).await.unwrap();
    let e = approve(&db, &maria, &big).await.unwrap_err();
    assert!(e.to_string().contains("aren't an approver"), "{e}");
    approve(&db, &robert, &big).await.unwrap();
    assert_eq!(status(s, &robert, &big).await, "awaiting_release");

    // Under the controller's threshold the step is skipped, so robert may.
    let small = bill(s, &ap, &v, "189801", 9_000).await;
    approve(&db, &robert, &small).await.unwrap();
    db.drop().await;
}

#[tokio::test]
async fn a_workflow_that_needs_one_person_twice_is_refused() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "workflow.propose",
        json!({ "workflow": { "steps": [
            { "id": "a", "type": "approve", "approvers": ["robert"] },
            { "id": "b", "type": "approve", "approvers": ["robert"], "min_amount_cents": 500_000 },
            { "id": "pay", "type": "pay" }
        ] } }),
    )
    .await
    .unwrap_err();
    let CommandError::Invalid(msg) = &e else {
        panic!("{e:?}")
    };
    assert!(msg.contains("step b"), "{msg}");
    db.drop().await;
}

#[tokio::test]
async fn whoever_uploaded_the_document_cant_approve_it_either() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let s = &db.state;
    let (robert, ap) = (person("robert"), agent("claude-ap"));
    let v = vendor(s, &ap, "Desert Plumbing").await;
    set_workflow(
        s,
        &ap,
        Some(&v),
        json!([
            { "id": "ok", "type": "approve" },
            { "id": "pay", "type": "pay", "release": "manual" }
        ]),
    )
    .await;
    let pdf = format!("%PDF-1.4\n% invoice {}\n%%EOF\n", uuid::Uuid::new_v4());
    let doc = commit(
        s,
        &robert,
        "document.upload",
        json!({ "filename": "dp.pdf",
                "content_base64": base64::engine::general_purpose::STANDARD.encode(pdf) }),
    )
    .await
    .unwrap()["result"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // An agent enters the bill from robert's upload.
    let b = commit(
        s,
        &ap,
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "DP-5521", "invoice_date": "2027-01-15",
                "amount_cents": 142_000, "document_id": doc }),
    )
    .await
    .unwrap()["result"]["bill"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    set_rule(&db, true).await.unwrap();
    let e = approve(&db, &robert, &b).await.unwrap_err();
    assert!(
        e.to_string().contains("you uploaded this bill's document"),
        "{e}"
    );
    approve(&db, &person("maria"), &b).await.unwrap();
    db.drop().await;
}
