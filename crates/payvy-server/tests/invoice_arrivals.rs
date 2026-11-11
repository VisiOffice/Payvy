//! The invoice stack: a vendor sending the same invoice again never makes
//! a second payable bill, and a copy carrying a different amount blocks
//! payment until a person decides it.

use payvy_server::commands::CommandError;
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, bill, commit, confirm, person, vendor};

async fn create(
    db: &TestDb,
    who: &payvy_server::Actor,
    vendor_id: &str,
    number: &str,
    cents: i64,
    extra: Value,
) -> Result<Value, CommandError> {
    let mut input = json!({ "vendor_id": vendor_id, "invoice_number": number,
        "invoice_date": "2027-01-15", "amount_cents": cents });
    for (k, v) in extra.as_object().unwrap() {
        input[k] = v.clone();
    }
    let r = commit(&db.state, who, "bill.create", input).await?;
    Ok(r["result"].clone())
}

async fn get(db: &TestDb, bill_id: &str) -> Value {
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

/// A vendor whose bills go straight to a manual pay step.
async fn pay_vendor(db: &TestDb, name: &str) -> String {
    let robert = person("robert");
    let v = vendor(&db.state, &robert, name).await;
    common::set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    v
}

#[tokio::test]
async fn a_resend_stacks_on_the_bill_and_is_never_payable_twice() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let robert = person("robert");
    let v = pay_vendor(&db, "Allied").await;
    let b = bill(&db.state, &robert, &v, "R260405861", 8_740).await;

    // The same invoice sent again: same bill, recorded, nothing changes.
    let again = create(&db, &robert, &v, "R260405861", 8_740, json!({}))
        .await
        .unwrap();
    assert_eq!(again["bill"]["id"], b);
    assert_eq!(again["arrival"]["kind"], "resend");
    assert_eq!(again["arrival"]["pending"], false);
    assert_eq!(again["received_count"], 2);
    assert_eq!(again["bill"]["amount_cents"], 8_740);
    assert!(again["pending_revision"].is_null());
    let arrivals = again["arrivals"].as_array().unwrap();
    assert_eq!(arrivals.len(), 2);
    assert_eq!(arrivals[1]["amount_cents"], 8_740);
    assert_eq!(arrivals[1]["decision"], "none");
    let received = again["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "bill.invoice_received")
        .expect("the resend is in the bill's history");
    assert_eq!(received["payload"]["received_count"], 2);
    assert_eq!(received["payload"]["amount_cents"], 8_740);

    // One bill in the list, marked as sent twice.
    let list = commit(&db.state, &robert, "bill.list", json!({ "vendor_id": v }))
        .await
        .unwrap();
    let bills = list["result"]["bills"].as_array().unwrap();
    assert_eq!(bills.len(), 1);
    assert_eq!(bills[0]["received_count"], 2);
    assert!(bills[0]["pending_revision_amount_cents"].is_null());

    // It pays once, at the original amount.
    confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap();
    confirm(
        &db.state,
        &robert,
        "bill.mark_paid",
        json!({ "bill_id": b, "paid_on": "2027-01-20", "method": "ach" }),
    )
    .await
    .unwrap();

    // Even a copy arriving after payment only stacks; it cannot re-pay.
    let third = create(&db, &robert, &v, "R260405861", 8_740, json!({}))
        .await
        .unwrap();
    assert_eq!(third["bill"]["id"], b);
    assert_eq!(third["received_count"], 3);
    assert_eq!(third["bill"]["state"], "paid");
    db.drop().await;
}

#[tokio::test]
async fn a_revised_invoice_blocks_payment_until_a_person_decides() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let robert = person("robert");
    let v = pay_vendor(&db, "Allied").await;
    let b = bill(&db.state, &robert, &v, "R260405861", 8_740).await;

    // Allied's revision: $87.40, then $37.49.
    let rev = create(&db, &robert, &v, "R260405861", 3_749, json!({}))
        .await
        .unwrap();
    assert_eq!(rev["arrival"]["kind"], "revision");
    assert_eq!(rev["arrival"]["pending"], true);
    assert_eq!(rev["pending_revision"]["amount_cents"], 3_749);
    assert_eq!(rev["pending_revision"]["current_amount_cents"], 8_740);
    assert_eq!(rev["bill"]["amount_cents"], 8_740);

    let list = commit(&db.state, &robert, "bill.list", json!({ "vendor_id": v }))
        .await
        .unwrap();
    assert_eq!(
        list["result"]["bills"][0]["pending_revision_amount_cents"],
        3_749
    );

    // An agent can't decide it, and the bill can't be paid undecided.
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "bill.revision_accept",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let e = confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("revised to $37.49"), "{e}");

    // Rejecting keeps the original amount, and payment opens again.
    let decided = commit(
        &db.state,
        &robert,
        "bill.revision_reject",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(decided["bill"]["amount_cents"], 8_740);
    assert!(decided["pending_revision"].is_null());
    assert_eq!(decided["arrivals"][1]["decision"], "rejected");
    assert_eq!(decided["arrivals"][1]["decided_by"], "robert");
    confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn accepting_a_revision_makes_the_latest_amount_and_lines_payable() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let robert = person("robert");
    let v = pay_vendor(&db, "Allied").await;
    let first = create(
        &db,
        &robert,
        &v,
        "R-9",
        8_740,
        json!({ "lines": [{ "description": "Haul", "amount_cents": 8_740 }] }),
    )
    .await
    .unwrap();
    let b = first["bill"]["id"].as_str().unwrap().to_owned();

    // Two revisions arrive; the newer supersedes the older undecided one.
    create(&db, &robert, &v, "R-9", 4_000, json!({}))
        .await
        .unwrap();
    let latest = create(
        &db,
        &robert,
        &v,
        "R-9",
        3_749,
        json!({ "lines": [{ "description": "Haul", "amount_cents": 3_749 }] }),
    )
    .await
    .unwrap();
    assert_eq!(latest["pending_revision"]["amount_cents"], 3_749);
    assert_eq!(latest["arrivals"][1]["decision"], "superseded");

    let accepted = commit(
        &db.state,
        &robert,
        "bill.revision_accept",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(accepted["bill"]["amount_cents"], 3_749);
    assert_eq!(accepted["lines"][0]["amount_cents"], 3_749);
    assert_eq!(accepted["arrivals"][2]["decision"], "accepted");
    assert!(accepted["pending_revision"].is_null());

    // Deciding again, with nothing waiting, is an error — never a no-op.
    let e = commit(
        &db.state,
        &robert,
        "bill.revision_accept",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    // The bill pays at the revised amount.
    let released = confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap();
    assert_eq!(released["result"]["bill"]["amount_cents"], 3_749);
    db.drop().await;
}

#[tokio::test]
async fn accepting_a_revision_waits_for_allocations_over_the_new_amount_to_be_reversed() {
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
    commit(
        &db.state,
        &robert,
        "person.add",
        json!({ "email": "robert@example.com", "name": "robert", "actor": "robert", "roles": ["admin"] }),
    )
    .await
    .unwrap();
    let v = pay_vendor(&db, "Allied").await;
    let po = commit(
        &db.state,
        &robert,
        "po.create",
        json!({ "vendor_id": v, "po_number": "PO-Q8", "owner": "robert", "authorized_cents": 100_000 }),
    )
    .await
    .unwrap()["result"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    commit(&db.state, &robert, "po.activate", json!({ "po_id": po }))
        .await
        .unwrap();
    let b = bill(&db.state, &robert, &v, "R-Q8", 8_740).await;
    confirm(
        &db.state,
        &robert,
        "po.match_apply",
        json!({ "bill_id": b, "allocations": [{ "po_id": po, "amount_cents": 8_000 }] }),
    )
    .await
    .unwrap();
    create(&db, &robert, &v, "R-Q8", 3_749, json!({}))
        .await
        .unwrap();

    // $80.00 is allocated against an $37.49 revision: acceptance waits
    // until the excess is reversed by its own record, never silently.
    let e = commit(
        &db.state,
        &robert,
        "bill.revision_accept",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(e.to_string().contains("po.allocation_reverse"), "{e}");
    assert_eq!(get(&db, &b).await["bill"]["amount_cents"], 8_740);

    let got = get(&db, &b).await;
    let allocation = got["purchase_orders"]["allocations"][0]["allocation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    commit(
        &db.state,
        &robert,
        "po.allocation_reverse",
        json!({ "allocation_id": allocation, "reason": "invoice revised below the allocation" }),
    )
    .await
    .unwrap();
    let accepted = commit(
        &db.state,
        &robert,
        "bill.revision_accept",
        json!({ "bill_id": b }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(accepted["bill"]["amount_cents"], 3_749);
    db.drop().await;
}

#[tokio::test]
async fn a_document_copy_files_with_the_existing_bill_not_a_new_one() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let v = pay_vendor(&db, "Allied").await;
    use base64::Engine;
    async fn upload(db: &TestDb, who: &payvy_server::Actor, name: &str) -> String {
        let pdf = format!("%PDF-1.4\n% invoice {}\n%%EOF\n", uuid::Uuid::new_v4());
        commit(
            &db.state,
            who,
            "document.upload",
            json!({ "filename": name,
                    "content_base64": base64::engine::general_purpose::STANDARD.encode(pdf) }),
        )
        .await
        .unwrap()["result"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    let doc1 = upload(&db, &robert, "first.pdf").await;
    let first = create(
        &db,
        &robert,
        &v,
        "R-55",
        8_740,
        json!({ "document_id": doc1 }),
    )
    .await
    .unwrap();
    let b = first["bill"]["id"].as_str().unwrap().to_owned();

    let doc2 = upload(&db, &robert, "again.pdf").await;
    let again = create(
        &db,
        &robert,
        &v,
        "R-55",
        8_740,
        json!({ "document_id": doc2.clone() }),
    )
    .await
    .unwrap();
    assert_eq!(again["bill"]["id"], b);
    assert_eq!(again["received_count"], 2);
    assert_eq!(again["arrivals"][1]["document_id"], doc2);
    assert_eq!(again["documents"].as_array().unwrap().len(), 2);
    db.drop().await;
}

#[tokio::test]
async fn allow_duplicate_still_makes_a_separate_recorded_bill() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let robert = person("robert");
    let v = pay_vendor(&db, "Allied").await;
    let b = bill(&db.state, &robert, &v, "R-77", 5_000).await;

    let other = create(
        &db,
        &robert,
        &v,
        "R-77",
        5_000,
        json!({ "allow_duplicate": true }),
    )
    .await
    .unwrap();
    assert_ne!(other["bill"]["id"], b);
    assert_eq!(other["received_count"], 1);
    assert!(other.get("arrival").is_none() || other["arrival"].is_null());

    let list = commit(&db.state, &robert, "bill.list", json!({ "vendor_id": v }))
        .await
        .unwrap();
    assert_eq!(list["result"]["bills"].as_array().unwrap().len(), 2);
    let got = get(&db, &b).await;
    assert_eq!(got["received_count"], 1);
    db.drop().await;
}
