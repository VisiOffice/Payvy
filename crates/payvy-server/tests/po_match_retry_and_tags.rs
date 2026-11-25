//! Phase 1: deterministic hint backfill from stored readings, `po.match_retry`
//! diagnosis, and person-tagged `vendor_field_examples`.

use payvy_server::Actor;
use payvy_server::commands::CommandError;
use payvy_store::{self as store, DEFAULT_ORG, ExtractionRead, NewExtraction};
use serde_json::{Value, json};
use uuid::Uuid;

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
        json!({ "email": "robert@example.com", "name": "Robert", "actor": "robert", "roles": ["admin"] }),
    )
    .await
    .unwrap();
}

async fn make_po(
    db: &TestDb,
    vendor_id: &str,
    po_number: &str,
    cents: i64,
    address: Option<&str>,
    period: Option<(&str, &str)>,
    activate: bool,
) -> String {
    let mut input = json!({
        "vendor_id": vendor_id,
        "po_number": po_number,
        "owner": "robert",
        "authorized_cents": cents,
    });
    if let Some(addr) = address {
        input["service_address"] = json!(addr);
    }
    if let Some((start, end)) = period {
        input["period_start"] = json!(start);
        input["period_end"] = json!(end);
    }
    let r = commit(&db.state, &person("robert"), "po.create", input)
        .await
        .unwrap();
    let id = r["result"]["id"].as_str().unwrap().to_owned();
    if activate {
        commit(
            &db.state,
            &person("robert"),
            "po.activate",
            json!({ "po_id": id }),
        )
        .await
        .unwrap();
    }
    id
}

async fn make_bill(
    db: &TestDb,
    vendor_id: &str,
    inv: &str,
    cents: i64,
    date: &str,
    po_number: Option<&str>,
    address: Option<&str>,
) -> String {
    let mut input = json!({
        "vendor_id": vendor_id,
        "invoice_number": inv,
        "invoice_date": date,
        "amount_cents": cents,
    });
    if let Some(p) = po_number {
        input["po_number"] = json!(p);
    }
    if let Some(a) = address {
        input["service_address"] = json!(a);
    }
    let r = commit(&db.state, &agent("ap-bot"), "bill.create", input)
        .await
        .unwrap();
    r["result"]["bill"]["id"].as_str().unwrap().to_owned()
}

/// Attaches a document row and a stored extraction directly to an existing bill,
/// simulating a bill created before hints were copied (or whose reading ran later).
async fn attach_reading(db: &TestDb, bill_id: &str, draft: Value, source_pages: Value) -> Uuid {
    let now = *db.clock.lock().unwrap();
    let bill_uuid = Uuid::parse_str(bill_id).unwrap();
    let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
    let doc_id = Uuid::new_v4();
    let hash = format!("hash-{doc_id}");
    sqlx::query(
        "INSERT INTO documents (id, org_id, sha256, content_type, size_bytes, filename, storage_key, source, status, bill_id, received_at, created_by, page_count) \
         VALUES ($1, $2, $3, 'application/pdf', 100, 'invoice.pdf', $3, 'upload', 'filed', $4, $5, 'robert', 1)",
    )
    .bind(doc_id)
    .bind(DEFAULT_ORG)
    .bind(&hash)
    .bind(bill_uuid)
    .bind(now)
    .execute(&mut *tx)
    .await
    .unwrap();
    let ex = store::insert_extraction(
        &mut tx,
        DEFAULT_ORG,
        &NewExtraction {
            document_id: doc_id,
            provider: "fake",
            model: "fake-model",
            outcome: Ok(ExtractionRead {
                answer: &draft,
                draft: &draft,
                checks: &json!([]),
                vendor_match: &json!({}),
                invoices: None,
            }),
            created_at: now,
            created_by: "reader",
            arrangement_version: 0,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE extractions SET analysis = $2 WHERE id = $1")
        .bind(ex.id)
        .bind(json!({ "source_pages": source_pages }))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    doc_id
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

/// Tests 1 & 2: A bill with NULL hints whose stored reading has the service address
/// is reserved by `po.match_retry` (event names the fill and its source); a hint
/// already on the bill is never overwritten.
#[tokio::test]
async fn match_retry_fills_missing_hints_from_reading_without_overwriting() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let v = vendor(&db.state, &person("robert"), "Acme Hauling").await;
    make_po(
        &db,
        &v,
        "PO-100",
        100_000,
        Some("120 Main St, Austin, TX"),
        None,
        true,
    )
    .await;

    // 1. Bill created with no hints, then has a stored reading carrying service_address
    // matching the bill's invoice_number and total_cents.
    let b1 = make_bill(&db, &v, "INV-1", 50_000, "2026-11-10", None, None).await;
    assert_eq!(
        get_bill(&db, &b1).await["purchase_orders"]["diagnosis"]["reason"],
        "no_hints"
    );
    attach_reading(
        &db,
        &b1,
        json!({
            "invoice_number": "INV-1",
            "total_cents": 50_000,
            "service_address": "120 Main Street, Austin, TX"
        }),
        json!([{ "page": 1, "method": "pdf_text", "text": "Service at 120 Main Street, Austin, TX" }]),
    )
    .await;

    let retried = commit(
        &db.state,
        &agent("ap-bot"),
        "po.match_retry",
        json!({ "bill_id": b1 }),
    )
    .await
    .unwrap();
    assert_eq!(retried["result"]["diagnosis"]["outcome"], "matched");
    assert_eq!(retried["result"]["diagnosis"]["po_number"], "PO-100");
    assert_eq!(retried["result"]["matched"]["rule"], "service_address");

    let detail = get_bill(&db, &b1).await;
    assert_eq!(
        detail["bill"]["service_address"],
        "120 Main Street, Austin, TX"
    );
    let fill_ev = detail["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "bill.po_hints_filled")
        .expect("bill.po_hints_filled event");
    assert_eq!(fill_ev["payload"]["source"], "stored_reading");
    assert_eq!(fill_ev["payload"]["fields"], json!(["service_address"]));

    // 2. Bill already has po_reference "WRONG-PO"; stored reading has "PO-100".
    // Retrying must NOT overwrite the existing po_reference.
    let b2 = make_bill(
        &db,
        &v,
        "INV-2",
        25_000,
        "2026-11-10",
        Some("WRONG-PO"),
        None,
    )
    .await;
    attach_reading(
        &db,
        &b2,
        json!({
            "invoice_number": "INV-2",
            "total_cents": 25_000,
            "po_number": "PO-100",
            "service_address": "120 Main Street"
        }),
        json!([]),
    )
    .await;
    let r2 = commit(
        &db.state,
        &agent("ap-bot"),
        "po.match_retry",
        json!({ "bill_id": b2 }),
    )
    .await
    .unwrap();
    assert_eq!(r2["result"]["diagnosis"]["outcome"], "not_matched");
    assert_eq!(r2["result"]["diagnosis"]["reason"], "po_number_names_none");
    let d2 = get_bill(&db, &b2).await;
    assert_eq!(d2["bill"]["po_reference"], "WRONG-PO");
    // Missing service_address was still filled without overwriting po_reference.
    assert_eq!(d2["bill"]["service_address"], "120 Main Street");

    // 2b. Fallback reading with a different invoice_number fills nothing and reserves nothing.
    let b_diff_inv = make_bill(&db, &v, "INV-DIFF", 20_000, "2026-11-10", None, None).await;
    attach_reading(
        &db,
        &b_diff_inv,
        json!({
            "invoice_number": "INV-OTHER",
            "total_cents": 20_000,
            "service_address": "120 Main Street, Austin, TX"
        }),
        json!([]),
    )
    .await;
    let r_diff_inv = commit(
        &db.state,
        &agent("ap-bot"),
        "po.match_retry",
        json!({ "bill_id": b_diff_inv }),
    )
    .await
    .unwrap();
    assert_eq!(r_diff_inv["result"]["diagnosis"]["outcome"], "not_matched");
    assert_eq!(r_diff_inv["result"]["diagnosis"]["reason"], "no_hints");
    assert!(r_diff_inv["result"]["matched"].is_null());
    assert!(get_bill(&db, &b_diff_inv).await["bill"]["service_address"].is_null());

    // 2c. Fallback reading with a different amount fills nothing and reserves nothing.
    let b_diff_amt = make_bill(&db, &v, "INV-AMT", 20_000, "2026-11-10", None, None).await;
    attach_reading(
        &db,
        &b_diff_amt,
        json!({
            "invoice_number": "INV-AMT",
            "total_cents": 20_001,
            "service_address": "120 Main Street, Austin, TX"
        }),
        json!([]),
    )
    .await;
    let r_diff_amt = commit(
        &db.state,
        &agent("ap-bot"),
        "po.match_retry",
        json!({ "bill_id": b_diff_amt }),
    )
    .await
    .unwrap();
    assert_eq!(r_diff_amt["result"]["diagnosis"]["outcome"], "not_matched");
    assert_eq!(r_diff_amt["result"]["diagnosis"]["reason"], "no_hints");
    assert!(r_diff_amt["result"]["matched"].is_null());
    assert!(get_bill(&db, &b_diff_amt).await["bill"]["service_address"].is_null());
}

/// Test 3: Every diagnosis reason is returned for a bill built to produce it.
#[tokio::test]
async fn diagnosis_returns_every_reason() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": "robert@example.com", "name": "Robert", "actor": "robert", "roles": ["admin"] }),
    )
    .await
    .unwrap();
    let v = vendor(&db.state, &person("robert"), "Diag Vendor").await;

    // purchase_orders_off
    let b_off = make_bill(&db, &v, "D-OFF", 10_000, "2026-11-10", None, None).await;
    let r_off = commit(
        &db.state,
        &person("robert"),
        "po.match_retry",
        json!({ "bill_id": b_off }),
    )
    .await
    .unwrap();
    assert_eq!(
        r_off["result"]["diagnosis"]["reason"],
        "purchase_orders_off"
    );

    // Turn POs on.
    commit(
        &db.state,
        &person("admin"),
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();

    // no_active_po
    let r_none = commit(
        &db.state,
        &person("robert"),
        "po.match_retry",
        json!({ "bill_id": b_off }),
    )
    .await
    .unwrap();
    assert_eq!(r_none["result"]["diagnosis"]["reason"], "no_active_po");

    // Create one active PO with period and amount.
    make_po(
        &db,
        &v,
        "PO-D1",
        50_000,
        Some("10 Oak Ave, Austin, TX"),
        Some(("2026-11-01", "2026-11-30")),
        true,
    )
    .await;

    // no_hints
    let r_hints = commit(
        &db.state,
        &person("robert"),
        "po.match_retry",
        json!({ "bill_id": b_off }),
    )
    .await
    .unwrap();
    assert_eq!(r_hints["result"]["diagnosis"]["reason"], "no_hints");
    assert_eq!(r_hints["result"]["diagnosis"]["candidate_count"], 1);

    // po_number_names_none
    let b_bad_po = make_bill(
        &db,
        &v,
        "D-BADPO",
        10_000,
        "2026-11-10",
        Some("NO-SUCH"),
        None,
    )
    .await;
    assert_eq!(
        get_bill(&db, &b_bad_po).await["purchase_orders"]["diagnosis"]["reason"],
        "po_number_names_none"
    );

    // address_names_none
    let b_bad_addr = make_bill(
        &db,
        &v,
        "D-BADADDR",
        10_000,
        "2026-11-10",
        None,
        Some("99 Elm St"),
    )
    .await;
    assert_eq!(
        get_bill(&db, &b_bad_addr).await["purchase_orders"]["diagnosis"]["reason"],
        "address_names_none"
    );

    // not_in_period
    let b_period = make_bill(
        &db,
        &v,
        "D-PERIOD",
        10_000,
        "2026-12-15",
        None,
        Some("10 Oak Ave"),
    )
    .await;
    let d_period = get_bill(&db, &b_period).await["purchase_orders"]["diagnosis"].clone();
    assert_eq!(d_period["reason"], "not_in_period");
    assert_eq!(d_period["po_number"], "PO-D1");

    // not_covered
    let b_over = make_bill(&db, &v, "D-OVER", 60_000, "2026-11-10", Some("PO-D1"), None).await;
    let d_over = get_bill(&db, &b_over).await["purchase_orders"]["diagnosis"].clone();
    assert_eq!(d_over["reason"], "not_covered");
    assert_eq!(d_over["po_number"], "PO-D1");

    // several_candidates
    make_po(
        &db,
        &v,
        "PO-D2",
        50_000,
        Some("10 Oak Avenue, Austin, TX"),
        Some(("2026-11-01", "2026-11-30")),
        true,
    )
    .await;
    let b_sev = make_bill(
        &db,
        &v,
        "D-SEV",
        10_000,
        "2026-11-10",
        None,
        Some("10 Oak St"),
    )
    .await;
    let d_sev = get_bill(&db, &b_sev).await["purchase_orders"]["diagnosis"].clone();
    assert_eq!(d_sev["reason"], "several_candidates");
    assert_eq!(d_sev["candidate_count"], 2);
}

/// Test 4: A PO that becomes active later picks up a bill whose hints were only
/// in its stored reading.
#[tokio::test]
async fn po_activating_later_fills_hints_from_reading_and_matches_bill() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let v = vendor(&db.state, &person("robert"), "Late PO Vendor").await;

    let b = make_bill(&db, &v, "INV-LATE", 40_000, "2026-11-10", None, None).await;
    attach_reading(
        &db,
        &b,
        json!({
            "invoice_number": "INV-LATE",
            "total_cents": 40_000,
            "service_address": "500 Cedar Blvd, Austin, TX"
        }),
        json!([]),
    )
    .await;

    // Draft PO created, then activated -> auto_match_vendor runs place_bill -> fills hints & matches!
    let po_id = make_po(
        &db,
        &v,
        "PO-LATE",
        100_000,
        Some("500 Cedar Boulevard"),
        None,
        false,
    )
    .await;
    let act = commit(
        &db.state,
        &person("robert"),
        "po.activate",
        json!({ "po_id": po_id }),
    )
    .await
    .unwrap();
    assert_eq!(act["result"]["auto_matched"][0]["bill_id"], b);
    assert_eq!(act["result"]["auto_matched"][0]["rule"], "service_address");

    let detail = get_bill(&db, &b).await;
    assert_eq!(
        detail["bill"]["service_address"],
        "500 Cedar Blvd, Austin, TX"
    );
    assert_eq!(detail["purchase_orders"]["covered_cents"], 40_000);
}

/// Tests 5, 6, 7, 8, 9:
/// - 5: Person matches by hand with evidence -> example stored, grounded when on doc, bill hint filled.
/// - 6: Quote that does not normalize to PO's own identifiers is refused.
/// - 7: `po.match_tag` and `po.example_retire` refuse an agent key.
/// - 8: Second bill of same vendor lacking hint is NOT matched by phase 1 alone.
/// - 9: Second organization sees none of the examples (RLS).
#[tokio::test]
async fn person_tagging_validation_permissions_phase1_boundary_and_rls() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let v = vendor(&db.state, &person("robert"), "Tagged Vendor").await;
    let po_id = make_po(
        &db,
        &v,
        "PO-TAG-1",
        200_000,
        Some("742 Evergreen Terrace"),
        None,
        true,
    )
    .await;

    let b1 = make_bill(&db, &v, "INV-T1", 50_000, "2026-11-10", None, None).await;
    attach_reading(
        &db,
        &b1,
        json!({}),
        json!([{ "page": 1, "method": "pdf_text", "text": "Service location: 742 Evergreen Ter. PO #PO-TAG-1" }]),
    )
    .await;

    // 6. Quote that does not normalize to PO's identifiers is refused.
    let bad = confirm(
        &db.state,
        &person("robert"),
        "po.match_apply",
        json!({
            "bill_id": b1,
            "allocations": [{ "po_id": po_id, "amount_cents": 50_000 }],
            "evidence": [{ "field": "service_address", "quote": "999 Wrong Street" }],
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(bad, CommandError::Invalid(_)), "{bad}");
    assert!(bad.to_string().contains("742 Evergreen Terrace"), "{bad}");

    // 5. Person matches by hand with grounded evidence -> example saved & bill hint filled.
    let applied = confirm(
        &db.state,
        &person("robert"),
        "po.match_apply",
        json!({
            "bill_id": b1,
            "allocations": [{ "po_id": po_id, "amount_cents": 50_000 }],
            "evidence": [{ "field": "service_address", "quote": "742 Evergreen Ter.", "page": 1 }],
        }),
    )
    .await
    .unwrap();
    let ex = &applied["result"]["examples"][0];
    let ex_id = ex["id"].as_str().unwrap().to_owned();
    assert_eq!(ex["field"], "service_address");
    assert_eq!(ex["value"], "742 evergreen");
    assert_eq!(ex["grounded"], true);
    assert_eq!(ex["source"], "person_tag");

    let d1 = get_bill(&db, &b1).await;
    assert_eq!(d1["bill"]["service_address"], "742 Evergreen Ter.");
    assert!(
        d1["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "bill.po_hints_filled" && e["payload"]["source"] == "person_tag")
    );

    // Vendor detail lists the live example.
    let v_detail = commit(
        &db.state,
        &person("robert"),
        "vendor.get",
        json!({ "vendor_id": v }),
    )
    .await
    .unwrap();
    assert_eq!(
        v_detail["result"]["field_examples"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Duplicate tag on the same bill (same field and normalized value) is refused as Conflict.
    let dup_tag = commit(
        &db.state,
        &person("robert"),
        "po.match_tag",
        json!({
            "bill_id": b1,
            "evidence": [{ "field": "service_address", "quote": "742 Evergreen Terrace" }],
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(dup_tag, CommandError::Conflict(_)), "{dup_tag}");

    // 7. `po.match_tag` and `po.example_retire` refuse an agent key.
    let err_tag = commit(
        &db.state,
        &agent("ap-bot"),
        "po.match_tag",
        json!({
            "bill_id": b1,
            "evidence": [{ "field": "po_reference", "quote": "PO-TAG-1" }],
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err_tag, CommandError::PersonOnly(_)), "{err_tag}");

    let err_retire = commit(
        &db.state,
        &agent("ap-bot"),
        "po.example_retire",
        json!({ "example_id": ex_id, "reason": "agent try" }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err_retire, CommandError::PersonOnly(_)),
        "{err_retire}"
    );

    // 8. Phase 1 boundary: a second bill of the same vendor whose stored reading lacks
    // the hint is NOT matched by phase 1 alone (examples are not used until phase 2).
    let b2 = make_bill(&db, &v, "INV-T2", 50_000, "2026-11-11", None, None).await;
    attach_reading(
        &db,
        &b2,
        json!({}),
        json!([{ "page": 1, "method": "pdf_text", "text": "Service location: 742 Evergreen Ter." }]),
    )
    .await;
    let r2 = commit(
        &db.state,
        &agent("ap-bot"),
        "po.match_retry",
        json!({ "bill_id": b2 }),
    )
    .await
    .unwrap();
    assert_eq!(r2["result"]["diagnosis"]["outcome"], "not_matched");
    assert_eq!(r2["result"]["diagnosis"]["reason"], "no_hints");
    assert!(r2["result"]["matched"].is_null());

    // 9. RLS: a second organization sees none of the examples and cannot retire them.
    let org2 = db.add_org().await;
    let other_admin = Actor::administrator(org2, "other-admin");
    commit(
        &db.state,
        &other_admin,
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
    let rls_err = commit(
        &db.state,
        &other_admin,
        "po.example_retire",
        json!({ "example_id": ex_id, "reason": "cross-org" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(rls_err, CommandError::NotFound(_)), "{rls_err}");

    let mut tx2 = db.state.store.begin(org2).await.unwrap();
    let rows2 = store::purchase_orders::list_live_vendor_field_examples(
        &mut tx2,
        Uuid::parse_str(&v).unwrap(),
    )
    .await
    .unwrap();
    assert!(rows2.is_empty());

    // Person can retire the example cleanly.
    commit(
        &db.state,
        &person("robert"),
        "po.example_retire",
        json!({ "example_id": ex_id, "reason": "wrong format" }),
    )
    .await
    .unwrap();
    let v_after = commit(
        &db.state,
        &person("robert"),
        "vendor.get",
        json!({ "vendor_id": v }),
    )
    .await
    .unwrap();
    assert!(
        v_after["result"]["field_examples"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
