//! document.submit_reading: a company's own agent reads the invoice and Payvy
//! checks it like its own reading. No model and no document store are needed:
//! the documents here are rows with a page count, as an upload leaves them.

use payvy_server::commands::CommandError;
use payvy_store::{self as store, NewDocument};
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, commit, entitle, hosted, person, vendor};

/// King Environmental's invoice 189727, as an agent would read it.
fn king_reading() -> Value {
    json!({
        "kind": "invoice", "vendor_name": "King Environmental LLC", "vendor_email": "ar@kingenv.com",
        "vendor_address": "12 Quarry Rd, Mesa AZ", "account_number": "004417", "invoice_number": "189727",
        "invoice_date": "2027-01-10", "due_date": "2027-02-09", "terms": "Net 30", "currency": "USD",
        "po_number": null, "subtotal": "425.00", "tax": "34.00", "total": "459.00",
        "lines": [
            { "description": "30 yd roll-off haul", "amount": "300.00" },
            { "description": "Disposal, 2.5 tons", "amount": "125.00" }
        ],
        "notes": null, "invoices": []
    })
}

async fn document(db: &TestDb, pages: i32) -> String {
    let now = *db.clock.lock().unwrap();
    let org = store::DEFAULT_ORG;
    let mut tx = db.state.store.begin(org).await.unwrap();
    let sha = uuid::Uuid::new_v4().simple().to_string();
    let doc = store::insert_document(
        &mut tx,
        org,
        &NewDocument {
            sha256: &sha,
            content_type: "application/pdf",
            size_bytes: 64,
            filename: "INV-189727.pdf",
            storage_key: &format!("{org}/{sha}.pdf"),
            source: "upload",
            email_id: None,
            received_at: now,
            created_by: "robert",
            page_count: Some(pages),
        },
    )
    .await
    .unwrap()
    .unwrap();
    tx.commit().await.unwrap();
    doc.id.to_string()
}

async fn king(db: &TestDb) -> String {
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "King Environmental").await;
    commit(
        &db.state,
        &robert,
        "vendor.add_account",
        json!({ "vendor_id": v, "account_number": "4417" }),
    )
    .await
    .unwrap();
    v
}

async fn submit(db: &TestDb, input: Value) -> Result<Value, CommandError> {
    commit(
        &db.state,
        &agent("quarry-ap"),
        "document.submit_reading",
        input,
    )
    .await
}

#[tokio::test]
async fn on_a_plan_without_the_reader_the_agents_reading_is_checked_and_becomes_the_bill() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    db.state = hosted(&db.state);
    // Bring your own agent: payments on, Payvy's reader off.
    entitle(&db.state, store::DEFAULT_ORG, false, true, 1)
        .await
        .unwrap();
    let v = king(&db).await;
    let doc = document(&db, 1).await;

    // Payvy's own reader is refused, and says what to do instead.
    let e = commit(
        &db.state,
        &agent("quarry-ap"),
        "document.extract",
        json!({ "document_id": doc }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::NotEntitled(_)), "{e}");
    assert!(e.to_string().contains("document.submit_reading"), "{e}");

    let r = submit(
        &db,
        json!({ "document_id": doc, "reading": king_reading(), "model": "gpt-5.1" }),
    )
    .await
    .unwrap();
    let out = &r["result"];
    assert_eq!(out["ready"], true, "{out:#}");
    assert_eq!(out["extraction"]["provider"], "agent");
    assert_eq!(out["extraction"]["model"], "gpt-5.1");
    assert_eq!(out["extraction"]["created_by"], "quarry-ap");
    assert_eq!(r["events"], json!(["document.extracted"]));
    let input = out["bill_input"].clone();
    assert_eq!(input["vendor_id"], v.as_str());
    assert_eq!(input["amount_cents"], 45_900);

    // It shows on the document like any reading, with no AI usage recorded.
    let usage = commit(&db.state, &person("robert"), "usage.summary", json!({}))
        .await
        .unwrap();
    assert_eq!(usage["result"]["attempts"], 0, "{usage:#}");

    // bill.create quotes it and records what was corrected.
    let mut corrected = input.clone();
    corrected["due_date"] = json!("2027-02-15");
    let made = commit(&db.state, &agent("quarry-ap"), "bill.create", corrected)
        .await
        .unwrap();
    assert!(made["result"]["bill"]["id"].is_string(), "{made:#}");

    // The same invoice again, as another document: flagged as a duplicate.
    let again = document(&db, 1).await;
    let r = submit(
        &db,
        json!({ "document_id": again, "reading": king_reading() }),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["ready"], false);
    assert!(
        r["result"]["failed_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "not_a_duplicate"),
        "{:#}",
        r["result"]
    );
    assert_eq!(r["result"]["extraction"]["model"], "unspecified");
}

#[tokio::test]
async fn a_weak_reading_fails_checks_rather_than_making_a_wrong_bill() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    king(&db).await;
    let doc = document(&db, 1).await;
    let mut reading = king_reading();
    // Lines that don't add up, and an amount that isn't a number.
    reading["lines"][1]["amount"] = json!("1,25.00");
    reading["total"] = json!("about 460");
    let r = submit(&db, json!({ "document_id": doc, "reading": reading }))
        .await
        .unwrap();
    assert_eq!(r["result"]["ready"], false);
    assert!(r["result"]["bill_input"].is_null(), "{:#}", r["result"]);
    assert!(
        !r["result"]["failed_checks"].as_array().unwrap().is_empty(),
        "{:#}",
        r["result"]
    );

    // Already read: refused unless again.
    let e = submit(
        &db,
        json!({ "document_id": doc, "reading": king_reading() }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    let r = submit(
        &db,
        json!({ "document_id": doc, "reading": king_reading(), "again": true }),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["ready"], true, "{:#}", r["result"]);
}

#[tokio::test]
async fn malformed_or_oversized_readings_are_refused_before_anything_is_written() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let doc = document(&db, 1).await;
    let mut unknown = king_reading();
    unknown["kind"] = json!("ransom_note");
    let mut many = king_reading();
    many["lines"] = json!(vec![json!({ "description": "x", "amount": "1.00" }); 1001]);
    for bad in [
        json!({ "document_id": doc, "reading": unknown }),
        json!({ "document_id": doc, "reading": many }),
        json!({ "document_id": doc, "reading": king_reading(), "model": "m".repeat(81) }),
        json!({ "document_id": doc, "reading": king_reading(), "extra": true }),
    ] {
        let e = submit(&db, bad).await.unwrap_err();
        assert_eq!(e.code(), "invalid_input", "{e}");
    }
    let r = commit(&db.state, &person("robert"), "document.list", json!({}))
        .await
        .unwrap();
    assert!(
        !r.to_string().contains("\"provider\":\"agent\""),
        "nothing was stored: {r:#}"
    );
}

#[tokio::test]
async fn several_invoices_in_one_file_are_split_as_with_payvys_reader() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    king(&db).await;
    let doc = document(&db, 2).await;
    let mut reading = king_reading();
    reading["invoices"] = json!([
        { "pages": [1], "invoice_number": "189727", "total": "459.00" },
        { "pages": [2], "invoice_number": "189728", "total": "120.00" }
    ]);
    let r = submit(&db, json!({ "document_id": doc, "reading": reading }))
        .await
        .unwrap();
    let out = &r["result"];
    assert_eq!(out["ready"], false, "one_invoice fails: {out:#}");
    assert_eq!(
        out["split"]["to_read"].as_array().map(Vec::len),
        Some(2),
        "{out:#}"
    );
}
