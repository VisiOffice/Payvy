//! Durable line review through the same command surface as REST/MCP.
mod common;
use base64::Engine;
use common::{TestDb, agent, commit, person, vendor};
use serde_json::{Value, json};
use uuid::Uuid;

async fn setup() -> Option<(TestDb, String, String)> {
    let db = TestDb::with_storage().await?;
    let who = person("reviewer");
    let v = vendor(&db.state, &who, "Review vendor").await;
    commit(
        &db.state,
        &who,
        "invoice_review.configure",
        json!({"enabled":true}),
    )
    .await
    .unwrap();
    let pdf = format!("%PDF-1.4\n{}\n%%EOF", Uuid::new_v4());
    let d=commit(&db.state,&who,"document.upload",json!({"filename":"review.pdf","content_base64":base64::engine::general_purpose::STANDARD.encode(pdf)})).await.unwrap();
    Some((db, v, d["result"]["id"].as_str().unwrap().into()))
}
fn payload(vendor: &str) -> Value {
    json!({
        "vendor_id":vendor,"invoice_number":"R-1","invoice_date":"2026-11-11","due_date":null,
        "total":"90.00","currency":"USD","memo":"","tax":"","tax_basis":"included","source_checked":true,
        "rows":[{"id":"service","description":"Service","amount":"100.00","source_index":null,"excluded_reason":null},
                {"id":"discount","description":"Discount","amount":"-10.00","source_index":null,"excluded_reason":null},
                {"id":"included","description":"Included visit","amount":"0.00","source_index":null,"excluded_reason":null}]
    })
}
fn save(doc: &str, vendor: &str) -> Value {
    json!({"document_id":doc,"extraction_id":null,"expected_version":0,"arrangement_version":0,"request_id":Uuid::new_v4(),"payload":payload(vendor)})
}
fn bill(doc: &str, vendor: &str, review: &Value) -> Value {
    json!({"document_id":doc,"review_id":review["result"]["review"]["id"],"vendor_id":vendor,"invoice_number":"R-1","invoice_date":"2026-11-11","amount_cents":9000})
}

#[tokio::test]
async fn signed_lines_are_saved_replayed_and_pinned_to_the_bill() {
    let Some((db, v, doc)) = setup().await else {
        return;
    };
    let who = agent("preparer");
    let input = save(&doc, &v);
    let preview = payvy_server::commands::run(
        &db.state,
        &who,
        "document.review_preview",
        input.clone(),
        payvy_server::commands::Mode::Commit {
            confirm_token: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(preview["result"]["validation"]["ready"], true);
    let before = commit(
        &db.state,
        &who,
        "document.review_get",
        json!({"document_id":doc}),
    )
    .await
    .unwrap();
    assert!(before["result"]["history"].as_array().unwrap().is_empty());
    let r = commit(&db.state, &who, "document.review_save", input.clone())
        .await
        .unwrap();
    let replay = commit(&db.state, &who, "document.review_save", input.clone())
        .await
        .unwrap();
    assert_eq!(r["result"], replay["result"]);
    assert_eq!(replay["events"], json!([]));
    let mut conflicting = input.clone();
    conflicting["payload"]["total"] = json!("95.00");
    assert!(
        commit(&db.state, &who, "document.review_save", conflicting)
            .await
            .is_err()
    );
    let mut missing = bill(&doc, &v, &r);
    missing.as_object_mut().unwrap().remove("review_id");
    assert!(
        commit(&db.state, &who, "bill.create", missing)
            .await
            .is_err()
    );
    let mut wrong = bill(&doc, &v, &r);
    wrong["amount_cents"] = json!(9500);
    assert!(commit(&db.state, &who, "bill.create", wrong).await.is_err());
    let b = commit(&db.state, &who, "bill.create", bill(&doc, &v, &r))
        .await
        .unwrap();
    assert_eq!(b["result"]["lines"][1]["amount_cents"], -1000);
    assert_eq!(b["result"]["lines"][2]["amount_cents"], 0);
    assert_eq!(b["result"]["bill"]["amount_cents"], 9000);
    assert_eq!(
        b["result"]["invoice_review"]["id"],
        r["result"]["review"]["id"]
    );
    let mut after = input;
    after["request_id"] = json!(Uuid::new_v4());
    after["expected_version"] = json!(1);
    assert!(
        commit(&db.state, &who, "document.review_save", after)
            .await
            .is_err()
    );
    db.drop().await;
}

#[tokio::test]
async fn unresolved_rows_require_a_persons_scoped_summary_and_edits_invalidate_it() {
    let Some((db, v, doc)) = setup().await else {
        return;
    };
    let ap = agent("preparer");
    let human = person("reviewer");
    let mut input = save(&doc, &v);
    input["payload"]["rows"][1]["amount"] = json!("");
    let r = commit(&db.state, &ap, "document.review_save", input.clone())
        .await
        .unwrap();
    assert_eq!(r["result"]["validation"]["ready"], false);
    assert!(
        commit(&db.state, &ap, "bill.create", bill(&doc, &v, &r))
            .await
            .is_err()
    );
    let accept = json!({"document_id":doc,"expected_version":1,"arrangement_version":0,"request_id":Uuid::new_v4(),"reason":"Only invoice total is legible; original reviewed"});
    assert!(
        commit(
            &db.state,
            &ap,
            "document.review_accept_summary",
            accept.clone()
        )
        .await
        .is_err()
    );
    let accepted = commit(&db.state, &human, "document.review_accept_summary", accept)
        .await
        .unwrap();
    input["request_id"] = json!(Uuid::new_v4());
    input["expected_version"] = json!(2);
    input["payload"]["memo"] = json!("Changed");
    let edited = commit(&db.state, &ap, "document.review_save", input.clone())
        .await
        .unwrap();
    assert!(edited["result"]["review"]["summary_reason"].is_null());
    assert!(
        commit(&db.state, &ap, "bill.create", bill(&doc, &v, &accepted))
            .await
            .is_err()
    );
    assert!(
        commit(&db.state, &ap, "bill.create", bill(&doc, &v, &edited))
            .await
            .is_err()
    );
    commit(
        &db.state,
        &human,
        "invoice_review.configure",
        json!({"enabled":false}),
    )
    .await
    .unwrap();
    let mut bypass = bill(&doc, &v, &edited);
    bypass.as_object_mut().unwrap().remove("review_id");
    assert!(commit(&db.state, &ap, "bill.create", bypass).await.is_err());
    let accepted=commit(&db.state,&human,"document.review_accept_summary",json!({"document_id":doc,"expected_version":3,"arrangement_version":0,"request_id":Uuid::new_v4(),"reason":"Confirmed header-only after the edit"})).await.unwrap();
    // A summary decision settles extraction only. It cannot waive live PO controls.
    commit(&db.state, &human, "po.configure", json!({"enabled":true}))
        .await
        .unwrap();
    common::set_workflow(
        &db.state,
        &human,
        Some(&v),
        json!([{"id":"pay","type":"pay","release":"manual"}]),
    )
    .await;
    commit(
        &db.state,
        &human,
        "po.set_vendor_settings",
        json!({"vendor_id":v,"require_po":true}),
    )
    .await
    .unwrap();
    let mut create = bill(&doc, &v, &accepted);
    create["memo"] = json!("Changed");
    let b = commit(&db.state, &ap, "bill.create", create).await.unwrap();
    assert!(b["result"]["lines"].as_array().unwrap().is_empty());
    assert!(b["result"]["invoice_review"]["summary_reason"].is_string());
    assert_eq!(b["result"]["purchase_orders"]["exception"]["kind"], "no_po");
    let err = common::confirm(
        &db.state,
        &human,
        "bill.release",
        json!({"bill_id":b["result"]["bill"]["id"]}),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("no_po"), "{err}");
    db.drop().await;
}

#[tokio::test]
async fn concurrent_edits_conflict_and_cross_company_review_links_are_refused() {
    let Some((db, v, doc)) = setup().await else {
        return;
    };
    let who = person("reviewer");
    let a = save(&doc, &v);
    let mut b = a.clone();
    b["request_id"] = json!(Uuid::new_v4());
    b["payload"]["memo"] = json!("other edit");
    let (a, b) = tokio::join!(
        commit(&db.state, &who, "document.review_save", a),
        commit(&db.state, &who, "document.review_save", b)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let other = db.add_org().await;
    let stranger = payvy_server::Actor::administrator(other, "other");
    assert!(
        commit(
            &db.state,
            &stranger,
            "document.review_get",
            json!({"document_id":doc})
        )
        .await
        .is_err()
    );
    let review = a.as_ref().ok().or(b.as_ref().ok()).unwrap()["result"]["review"]["id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let other_vendor = vendor(&db.state, &stranger, "Other vendor").await;
    let other_bill = commit(&db.state, &stranger, "bill.create", json!({"vendor_id":other_vendor,"invoice_number":"OTHER","invoice_date":"2026-11-11","amount_cents":100})).await.unwrap();
    let other_bill_id = other_bill["result"]["bill"]["id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let mut tx = db.state.store.begin(other).await.unwrap();
    let failed =
        sqlx::query("INSERT INTO bill_invoice_reviews(org_id,bill_id,review_id) VALUES ($1,$2,$3)")
            .bind(other)
            .bind(other_bill_id)
            .bind(review)
            .execute(&mut *tx)
            .await;
    assert!(failed.is_err());
    tx.rollback().await.unwrap();
    db.drop().await;
}

#[tokio::test]
async fn changed_pages_and_removed_source_rows_cannot_sneak_through() {
    let Some((db, v, doc)) = setup().await else {
        return;
    };
    let who = person("reviewer");
    let raw = json!({"kind":"invoice","vendor_name":"Review vendor","invoice_number":"R-1","invoice_date":"2026-11-11","currency":"USD","total":"90.00","lines":[{"description":"Service","amount":"100.00"},{"description":"Discount","amount":"-10.00"}]});
    let read: payvy_domain::extraction::Read = serde_json::from_value(raw.clone()).unwrap();
    let (draft, checks) = payvy_domain::extraction::interpret(&read);
    let mut tx = db.state.store.begin(who.org_id).await.unwrap();
    let ex = payvy_store::insert_extraction(
        &mut tx,
        who.org_id,
        &payvy_store::NewExtraction {
            document_id: doc.parse().unwrap(),
            provider: "fixture",
            model: "fixture",
            outcome: Ok(payvy_store::ExtractionRead {
                answer: &raw,
                draft: &json!(draft),
                checks: &json!(checks),
                vendor_match: &json!({"result":"no_match"}),
                invoices: None,
            }),
            created_at: (db.state.clock)(),
            created_by: "reader",
            arrangement_version: 0,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut input = save(&doc, &v);
    input["extraction_id"] = json!(ex.id);
    input["payload"]["rows"][0]["source_index"] = json!(0);
    input["payload"]["rows"][1]["source_index"] = json!(1);
    let mut removed = input.clone();
    removed["payload"]["rows"].as_array_mut().unwrap().remove(1);
    assert!(
        commit(&db.state, &who, "document.review_save", removed)
            .await
            .is_err()
    );
    let r = commit(&db.state, &who, "document.review_save", input)
        .await
        .unwrap();
    let mut tx = db.state.store.begin(who.org_id).await.unwrap();
    // Alter the stored page-count only so arrange can work with the tiny test PDF.
    sqlx::query("UPDATE documents SET page_count=1 WHERE id=$1")
        .bind(doc.parse::<Uuid>().unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    commit(
        &db.state,
        &who,
        "document.arrange",
        json!({"document_id":doc,"expected_version":0,"pages":[{"page":1,"rotation":90}]}),
    )
    .await
    .unwrap();
    assert!(
        commit(&db.state, &who, "bill.create", bill(&doc, &v, &r))
            .await
            .is_err()
    );
    db.drop().await;
}

#[tokio::test]
async fn split_and_undo_cannot_unenroll_previously_reviewed_pages() {
    let Some((db, v, doc)) = setup().await else {
        return;
    };
    let who = person("reviewer");
    let mut tx = db.state.store.begin(who.org_id).await.unwrap();
    sqlx::query("UPDATE documents SET page_count=2 WHERE id=$1")
        .bind(doc.parse::<Uuid>().unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let split = commit(
        &db.state,
        &who,
        "document.split",
        json!({"document_id":doc,"expected_version":0,"groups":[[1]]}),
    )
    .await
    .unwrap();
    // The command returns full document detail, including its new children.
    let child = split["result"]["split_into"][0]["id"].as_str().unwrap();
    let mut input = save(child, &v);
    input["arrangement_version"] = json!(1);
    commit(&db.state, &who, "document.review_save", input)
        .await
        .unwrap();
    commit(
        &db.state,
        &who,
        "invoice_review.configure",
        json!({"enabled":false}),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &who,
        "document.unsplit",
        json!({"document_id":child}),
    )
    .await
    .unwrap();
    let original = commit(
        &db.state,
        &who,
        "document.review_get",
        json!({"document_id":doc}),
    )
    .await
    .unwrap();
    assert_eq!(original["result"]["required"], true);
    assert!(commit(&db.state,&who,"bill.create",json!({"document_id":doc,"vendor_id":v,"invoice_number":"BYPASS","invoice_date":"2026-11-11","amount_cents":9000})).await.is_err());
    db.drop().await;
}
