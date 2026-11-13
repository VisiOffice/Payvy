//! Reading bills from documents, against a real database and store and a
//! fake model API that answers like Claude's Messages API or an
//! OpenAI-compatible one. No real model is called.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use base64::Engine;
use payvy_extract::{Config, Extractor, Provider};
use payvy_server::commands::CommandError;
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, commit, person, vendor};

/// Answers queued by the test, and the requests it received.
type ReadGate = (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
#[derive(Clone, Default)]
struct Fake {
    gate: Arc<Mutex<Option<ReadGate>>>,
    answers: Arc<Mutex<Vec<(StatusCode, Value)>>>,
    seen: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
}

impl Fake {
    fn answer(&self, status: StatusCode, body: Value) {
        self.answers.lock().unwrap().push((status, body));
    }

    fn requests(&self) -> Vec<(HeaderMap, Value)> {
        self.seen.lock().unwrap().clone()
    }
}

async fn reply(
    State(fake): State<Fake>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    fake.seen.lock().unwrap().push((headers, body));
    let gate = fake.gate.lock().unwrap().clone();
    if let Some((started, resume)) = gate {
        started.notify_one();
        resume.notified().await;
    }
    let next = {
        let mut q = fake.answers.lock().unwrap();
        (!q.is_empty()).then(|| q.remove(0))
    };
    let (status, body) = next.unwrap_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({ "error": { "message": "no answer queued" } }),
    ));
    (status, Json(body))
}

/// Starts the fake API and returns it with its base URL.
async fn fake_api() -> (Fake, String) {
    let fake = Fake::default();
    let app = axum::Router::new()
        .route("/v1/messages", post(reply))
        .route("/v1/chat/completions", post(reply))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, format!("http://{addr}"))
}

async fn db_with(provider: Provider) -> Option<(TestDb, Fake)> {
    let mut db = TestDb::with_storage().await?;
    let (fake, base) = fake_api().await;
    let (base_url, model) = match provider {
        Provider::Anthropic => (base, "claude-opus-5-5"),
        Provider::OpenAiCompatible => (format!("{base}/v1"), "llama-local"),
    };
    let extractor = Extractor::new(Config {
        provider,
        base_url,
        api_key: Some("test-model-key".into()),
        model: model.into(),
    })
    .unwrap();
    db.state = db.state.clone().with_extractor(extractor);
    Some((db, fake))
}

/// King Environmental's invoice 189727, as a model would read it.
fn king_invoice() -> Value {
    json!({
        "kind": "invoice", "vendor_name": "King Environmental LLC", "vendor_email": "ar@kingenv.com",
        "vendor_address": "12 Quarry Rd, Mesa AZ", "account_number": "004417", "invoice_number": "189727",
        "invoice_date": "2027-01-10", "due_date": "2027-02-09", "terms": "Net 30", "currency": "USD",
        "po_number": null, "subtotal": "425.00", "tax": "34.00", "total": "459.00",
        "lines": [
            { "description": "30 yd roll-off haul", "amount": "300.00" },
            { "description": "Disposal, 2.5 tons", "amount": "125.00" }
        ],
        "notes": null
    })
}

fn claude_says(answer: &Value) -> Value {
    json!({ "model": "claude-opus-5-5", "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": answer.to_string() }] })
}

fn openai_says(answer: &Value) -> Value {
    json!({ "model": "llama-local", "choices": [{ "finish_reason": "stop",
            "message": { "role": "assistant", "content": answer.to_string() } }] })
}

async fn upload(db: &TestDb) -> String {
    let pdf = format!("%PDF-1.4\n% {}\n%%EOF\n", uuid::Uuid::new_v4());
    let up = commit(
        &db.state,
        &person("robert"),
        "document.upload",
        json!({ "filename": "INV-189727.pdf",
                "content_base64": base64::engine::general_purpose::STANDARD.encode(pdf) }),
    )
    .await
    .unwrap();
    up["result"]["id"].as_str().unwrap().to_owned()
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

#[tokio::test]
async fn claude_reads_a_bill_and_corrections_are_recorded() {
    let Some((db, fake)) = db_with(Provider::Anthropic).await else {
        return;
    };
    let ap = agent("claude-ap");
    let king = king(&db).await;
    let doc = upload(&db).await;

    fake.answer(StatusCode::OK, claude_says(&king_invoice()));
    let out = commit(
        &db.state,
        &ap,
        "document.extract",
        json!({ "document_id": doc }),
    )
    .await
    .unwrap();
    assert_eq!(out["events"], json!(["document.extracted"]));
    let r = &out["result"];
    assert_eq!(r["ready"], true, "{r:#}");
    assert_eq!(r["extraction"]["status"], "read");
    assert_eq!(r["extraction"]["model"], "claude-opus-5-5");
    assert_eq!(r["extraction"]["provider"], "anthropic");
    assert_eq!(r["extraction"]["vendor_match"]["result"], "matched");
    assert_eq!(r["extraction"]["vendor_match"]["confidence"], "high");
    let input = r["bill_input"].clone();
    assert_eq!(input["vendor_id"], king.as_str());
    assert_eq!(input["amount_cents"], 45_900);
    assert_eq!(input["due_date"], "2027-02-09");
    assert_eq!(input["lines"].as_array().unwrap().len(), 3);

    // What Claude was sent: the PDF as a document block, the schema, the key.
    let (headers, body) = fake.requests().remove(0);
    assert_eq!(headers["x-api-key"], "test-model-key");
    assert_eq!(headers["anthropic-version"], "2026-10-21");
    assert_eq!(body["messages"][0]["content"][0]["type"], "document");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");

    // Read once unless asked again.
    let e = commit(
        &db.state,
        &ap,
        "document.extract",
        json!({ "document_id": doc }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    // The agent enters it with one field corrected; the log says which.
    let mut entered = input.clone();
    entered["invoice_date"] = json!("2027-01-11");
    let bill = commit(&db.state, &ap, "bill.create", entered)
        .await
        .unwrap();
    let bill_id = bill["result"]["bill"]["id"].as_str().unwrap().to_owned();
    assert_eq!(bill["result"]["lines"].as_array().unwrap().len(), 3);
    let log = commit(
        &db.state,
        &ap,
        "event.list",
        json!({ "entity_id": bill_id }),
    )
    .await
    .unwrap();
    let created = log["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "bill.created")
        .unwrap()
        .clone();
    assert_eq!(created["payload"]["corrected"], json!(["invoice_date"]));
    assert_eq!(created["payload"]["extraction_id"], input["extraction_id"]);

    // The document is filed, and still shows how it was read.
    let got = commit(
        &db.state,
        &ap,
        "document.get",
        json!({ "document_id": doc }),
    )
    .await
    .unwrap();
    assert_eq!(got["result"]["document"]["status"], "filed");
    assert_eq!(got["result"]["extractions"].as_array().unwrap().len(), 1);
    db.drop().await;
}

#[tokio::test]
async fn an_extraction_only_enters_the_document_it_read() {
    let Some((db, fake)) = db_with(Provider::Anthropic).await else {
        return;
    };
    let ap = agent("claude-ap");
    king(&db).await;
    let (doc, other) = (upload(&db).await, upload(&db).await);
    fake.answer(StatusCode::OK, claude_says(&king_invoice()));
    let out = commit(
        &db.state,
        &ap,
        "document.extract",
        json!({ "document_id": doc }),
    )
    .await
    .unwrap();
    let mut input = out["result"]["bill_input"].clone();
    input["document_id"] = json!(other);
    let e = commit(&db.state, &ap, "bill.create", input)
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn a_bill_already_entered_is_flagged() {
    let Some((db, fake)) = db_with(Provider::Anthropic).await else {
        return;
    };
    let ap = agent("claude-ap");
    let king = king(&db).await;
    common::bill(&db.state, &ap, &king, "0189727", 45_900).await;
    let doc = upload(&db).await;

    // Preview doesn't call the model.
    let p = payvy_server::commands::run(
        &db.state,
        &ap,
        "document.extract",
        json!({ "document_id": doc }),
        payvy_server::commands::Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(p["result"]["model"], "claude-opus-5-5");
    assert!(fake.requests().is_empty());

    fake.answer(StatusCode::OK, claude_says(&king_invoice()));
    let out = commit(
        &db.state,
        &ap,
        "document.extract",
        json!({ "document_id": doc }),
    )
    .await
    .unwrap();
    assert_eq!(out["result"]["ready"], false);
    assert_eq!(out["result"]["failed_checks"], json!(["not_a_duplicate"]));
    db.drop().await;
}

#[tokio::test]
async fn the_reader_records_failures_and_retries_later() {
    let Some((db, fake)) = db_with(Provider::OpenAiCompatible).await else {
        return;
    };
    let doc = upload(&db).await;

    fake.answer(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({ "error": { "message": "model is loading" } }),
    );
    assert_eq!(
        payvy_server::extract_worker::tick(&db.state).await.unwrap(),
        1
    );
    let got = commit(
        &db.state,
        &person("robert"),
        "document.get",
        json!({ "document_id": doc }),
    )
    .await
    .unwrap();
    let first = &got["result"]["extractions"][0]["extraction"];
    assert_eq!(first["status"], "failed");
    assert_eq!(first["created_by"], "extractor");
    assert!(first["error"].as_str().unwrap().contains("503"), "{first}");

    assert!(
        !first["error"]
            .as_str()
            .unwrap()
            .contains("model is loading")
    );

    // It waits before trying again.
    assert_eq!(
        payvy_server::extract_worker::tick(&db.state).await.unwrap(),
        0
    );
    db.advance(1);

    // A statement from a vendor nobody has set up: read, but not ready.
    let mut statement = king_invoice();
    statement["kind"] = json!("statement");
    statement["account_number"] = Value::Null;
    fake.answer(StatusCode::OK, openai_says(&statement));
    assert_eq!(
        payvy_server::extract_worker::tick(&db.state).await.unwrap(),
        1
    );
    let got = commit(
        &db.state,
        &person("robert"),
        "document.get",
        json!({ "document_id": doc }),
    )
    .await
    .unwrap();
    let latest = &got["result"]["extractions"][0];
    assert_eq!(latest["extraction"]["status"], "read");
    assert_eq!(
        latest["extraction"]["provider"],
        "openai-compatible (127.0.0.1)"
    );
    assert_eq!(latest["ready"], false);
    assert_eq!(latest["failed_checks"], json!(["is_invoice", "vendor"]));
    assert_eq!(latest["new_vendor"]["name"], "King Environmental LLC");
    assert!(latest["bill_input"].is_null());
    let (headers, body) = fake.requests().pop().unwrap();
    assert_eq!(headers["authorization"], "Bearer test-model-key");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);

    // Read now, so the reader leaves it alone.
    db.advance(1);
    assert_eq!(
        payvy_server::extract_worker::tick(&db.state).await.unwrap(),
        0
    );
    db.drop().await;
}

#[tokio::test]
async fn without_a_model_it_says_so() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let doc = upload(&db).await;
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "document.extract",
        json!({ "document_id": doc }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::NotConfigured(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn the_eval_scores_answers_against_the_fixtures() {
    // No database needed; the fake answers every document as King's invoice,
    // so King's fields are right and the other documents' mostly aren't.
    let (fake, base) = fake_api().await;
    let mut king = king_invoice();
    king["invoice_date"] = json!("2026-11-01");
    king["due_date"] = json!("2026-12-01");
    for _ in 0..5 {
        fake.answer(StatusCode::OK, claude_says(&king));
    }
    let extractor = Extractor::new(Config {
        provider: Provider::Anthropic,
        base_url: base,
        api_key: Some("k".into()),
        model: "claude-opus-5-5".into(),
    })
    .unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/invoices");
    let score = payvy_server::eval::run(&extractor, &dir).await.unwrap();
    assert_eq!(score.failed_documents, 0);
    assert_eq!(score.total, 10 + 10 + 8 + 3 + 4);
    // Row count alone cannot pass the full-row expectation: the fake omits
    // the service location from King's description.
    assert_eq!(score.right, 11);
    assert!(
        score.right >= 9 && score.right < score.total,
        "{}",
        score.right
    );
}

fn rich_invoice() -> Value {
    let mut read = king_invoice();
    read["vendor_name"] = json!("Synthetic Services");
    read["invoice_number"] = json!("V2-100");
    read["invoice_date"] = json!("2026-11-11");
    read["due_date"] = Value::Null;
    read["subtotal"] = Value::Null;
    read["tax"] = Value::Null;
    read["total"] = json!("30.86");
    read["coverage"] = json!({"pages_read":[1],"complete":true,"reason":null});
    read["lines"] = json!([
        {"description":"Disposal","amount":"30.86","details":{"quantity":"2.5","unit":"ton","unit_price":"12.345"}},
        {"description":"Included visit","amount":"0.00","details":{"quantity":"1","unit":"visit","unit_price":"0.00"}}
    ]);
    for row in read["lines"].as_array_mut().unwrap() {
        let quote = format!(
            "{} {} {} {} {}",
            row["description"].as_str().unwrap(),
            row["details"]["quantity"].as_str().unwrap(),
            row["details"]["unit"].as_str().unwrap(),
            row["details"]["unit_price"].as_str().unwrap(),
            row["amount"].as_str().unwrap()
        );
        let mut states = serde_json::Map::new();
        let mut evidence = Vec::new();
        for field in payvy_domain::invoice_evidence::FIELDS {
            let present = if ["description", "amount"].contains(field) {
                true
            } else {
                row["details"][field].is_string()
            };
            states.insert(
                (*field).into(),
                json!(if present { "present" } else { "absent" }),
            );
            if present {
                evidence.push(json!({"field":field,"page":1,"quote":quote}));
            }
        }
        row["details"]["states"] = json!(states);
        row["details"]["evidence"] = json!(evidence);
    }
    read
}
async fn upload_rich(db: &TestDb) -> String {
    let bytes = include_bytes!("../../../fixtures/invoices-v2/fractional.pdf");
    let r = commit(&db.state,&person("reviewer"),"document.upload",json!({"filename":"fractional.pdf","content_base64":base64::engine::general_purpose::STANDARD.encode(bytes)})).await.unwrap();
    r["result"]["id"].as_str().unwrap().into()
}
#[tokio::test]
async fn richer_review_gates_arithmetic_evidence_and_retains_immutable_metadata() {
    let Some((db, fake)) = db_with(Provider::OpenAiCompatible).await else {
        return;
    };
    let who = person("reviewer");
    let v = vendor(&db.state, &who, "Synthetic Services").await;
    commit(
        &db.state,
        &who,
        "invoice_review.configure",
        json!({"enabled":true,"extraction_v2_enabled":true}),
    )
    .await
    .unwrap();
    let doc = upload_rich(&db).await;
    fake.answer(StatusCode::OK, openai_says(&rich_invoice()));
    let ex = commit(
        &db.state,
        &who,
        "document.extract",
        json!({"document_id":doc}),
    )
    .await
    .unwrap();
    assert_eq!(ex["result"]["extraction"]["analysis"]["schema_version"], 2);
    assert_eq!(ex["result"]["ready"], false);
    assert_eq!(ex["result"]["review_required"], true);
    assert_eq!(ex["result"]["extraction"]["analysis"]["coverage_ok"], true);
    let get = commit(
        &db.state,
        &who,
        "document.review_get",
        json!({"document_id":doc}),
    )
    .await
    .unwrap();
    let mut p = get["result"]["initial"].clone();
    p["vendor_id"] = json!(v);
    p["source_checked"] = json!(true);
    // Explain the original explicitly, so the contract holds without local OCR tools too.
    for row in p["rows"].as_array_mut().unwrap() {
        row["source_resolution"] = json!("Checked all fields against the original PDF");
    }
    let mut input = json!({"document_id":doc,"extraction_id":get["result"]["extraction_id"],"expected_version":0,"arrangement_version":0,"request_id":uuid::Uuid::new_v4(),"payload":p});
    let first = commit(&db.state, &who, "document.review_save", input.clone())
        .await
        .unwrap();
    assert_eq!(first["result"]["validation"]["ready"], true);
    input["expected_version"] = json!(1);
    input["request_id"] = json!(uuid::Uuid::new_v4());
    input["payload"]["rows"][0]["details"]["quantity"] = json!("3");
    let preview = commit(&db.state, &who, "document.review_preview", input.clone())
        .await
        .unwrap();
    assert_eq!(preview["result"]["validation"]["ready"], false);
    let second = commit(&db.state, &who, "document.review_save", input.clone())
        .await
        .unwrap();
    assert_eq!(
        preview["result"]["validation"],
        second["result"]["validation"]
    );
    assert!(second["result"]["review"]["payload"]["rows"][0]["source_resolution"].is_null());
    let bill = json!({"vendor_id":v,"document_id":doc,"review_id":second["result"]["review"]["id"],"invoice_number":"V2-100","invoice_date":"2026-11-11","amount_cents":3086});
    assert!(
        commit(&db.state, &who, "bill.create", bill.clone())
            .await
            .is_err()
    );
    input["payload"] = second["result"]["review"]["payload"].clone();
    input["payload"]["rows"][0]["quantity_resolution"] =
        json!("Printed quantity corrected; supplier's printed total includes a bundle adjustment");
    input["payload"]["rows"][0]["source_resolution"] =
        json!("Checked changed quantity and printed amount against the original");
    input["expected_version"] = json!(2);
    input["request_id"] = json!(uuid::Uuid::new_v4());
    let third = commit(&db.state, &who, "document.review_save", input.clone())
        .await
        .unwrap();
    assert_eq!(third["result"]["validation"]["ready"], true);
    // Dropping details cannot silently erase extracted quantities or their validation.
    input["payload"]["rows"][0]["details"] = Value::Null;
    input["expected_version"] = json!(3);
    input["request_id"] = json!(uuid::Uuid::new_v4());
    let omitted = commit(&db.state, &who, "document.review_preview", input)
        .await
        .unwrap();
    assert_eq!(omitted["result"]["validation"]["ready"], false);
    commit(
        &db.state,
        &who,
        "invoice_review.configure",
        json!({"enabled":false}),
    )
    .await
    .unwrap();
    let mut missing = bill.clone();
    missing.as_object_mut().unwrap().remove("review_id");
    assert!(
        commit(&db.state, &who, "bill.create", missing)
            .await
            .is_err()
    );
    let mut accepted = bill;
    accepted["review_id"] = third["result"]["review"]["id"].clone();
    let b = commit(&db.state, &who, "bill.create", accepted)
        .await
        .unwrap();
    assert_eq!(b["result"]["lines"][0]["amount_cents"], 3086);
    assert_eq!(
        b["result"]["invoice_review"]["payload"]["rows"][0]["details"]["unit_price"],
        "12.345"
    );
    assert_eq!(
        b["result"]["invoice_review"]["payload"]["rows"][0]["details"]["quantity"],
        "3"
    );
    let history = commit(
        &db.state,
        &who,
        "document.review_get",
        json!({"document_id":doc}),
    )
    .await
    .unwrap();
    assert_eq!(
        history["result"]["history"][2]["payload"]["rows"][0]["details"]["quantity"],
        "2.5"
    );
    db.drop().await;
}

#[tokio::test]
async fn incomplete_page_coverage_needs_explicit_review_even_after_disabling() {
    let Some((db, fake)) = db_with(Provider::OpenAiCompatible).await else {
        return;
    };
    let who = person("reviewer");
    let v = vendor(&db.state, &who, "Synthetic Services").await;
    commit(
        &db.state,
        &who,
        "invoice_review.configure",
        json!({"enabled":true,"extraction_v2_enabled":true}),
    )
    .await
    .unwrap();
    let doc = upload_rich(&db).await;
    let mut read = rich_invoice();
    read["coverage"] = json!({"complete":false,"pages_read":[],"reason":"Unreadable table"});
    fake.answer(StatusCode::OK, openai_says(&read));
    commit(
        &db.state,
        &who,
        "document.extract",
        json!({"document_id":doc}),
    )
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
    let get = commit(
        &db.state,
        &who,
        "document.review_get",
        json!({"document_id":doc}),
    )
    .await
    .unwrap();
    assert_eq!(get["result"]["required"], true);
    let mut p = get["result"]["initial"].clone();
    p["vendor_id"] = json!(v);
    p["source_checked"] = json!(true);
    for row in p["rows"].as_array_mut().unwrap() {
        row["source_resolution"] = json!("Checked original");
    }
    let mut input = json!({"document_id":doc,"extraction_id":get["result"]["extraction_id"],"expected_version":0,"arrangement_version":0,"request_id":uuid::Uuid::new_v4(),"payload":p});
    let r = commit(&db.state, &who, "document.review_save", input.clone())
        .await
        .unwrap();
    assert_eq!(r["result"]["validation"]["header_ready"], false);
    assert!(commit(&db.state,&who,"document.review_accept_summary",json!({"document_id":doc,"expected_version":1,"arrangement_version":0,"request_id":uuid::Uuid::new_v4(),"reason":"Only summary available"})).await.is_err());
    input["expected_version"] = json!(1);
    input["request_id"] = json!(uuid::Uuid::new_v4());
    input["payload"]["coverage_resolution"] =
        json!("Checked the single selected page, including both rows and the printed total");
    let r = commit(&db.state, &who, "document.review_save", input)
        .await
        .unwrap();
    assert_eq!(r["result"]["validation"]["ready"], true);
    db.drop().await;
}

#[tokio::test]
async fn model_wait_releases_document_lock_and_stale_results_stay_history_only() {
    let Some((db, fake)) = db_with(Provider::OpenAiCompatible).await else {
        return;
    };
    let who = person("reviewer");
    commit(
        &db.state,
        &who,
        "invoice_review.configure",
        json!({"enabled":true,"extraction_v2_enabled":true}),
    )
    .await
    .unwrap();
    let doc = upload_rich(&db).await;
    let started = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *fake.gate.lock().unwrap() = Some((started.clone(), resume.clone()));
    fake.answer(StatusCode::OK, openai_says(&rich_invoice()));
    let state = db.state.clone();
    let actor = who.clone();
    let d = doc.clone();
    let reading = tokio::spawn(async move {
        commit(&state, &actor, "document.extract", json!({"document_id":d})).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
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
    let required = commit(
        &db.state,
        &who,
        "document.review_get",
        json!({"document_id":doc}),
    )
    .await
    .unwrap();
    assert_eq!(required["result"]["required"], true);
    let duplicate = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        commit(
            &db.state,
            &who,
            "document.extract",
            json!({"document_id":doc}),
        ),
    )
    .await
    .unwrap();
    assert!(matches!(duplicate, Err(CommandError::Conflict(_))));
    assert_eq!(fake.requests().len(), 1);
    let arranged = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        commit(
            &db.state,
            &who,
            "document.arrange",
            json!({"document_id":doc,"expected_version":0,"pages":[{"page":1,"rotation":90}]}),
        ),
    )
    .await;
    resume.notify_one();
    arranged
        .expect("model must not hold document lock")
        .unwrap();
    let out = reading.await.unwrap().unwrap();
    assert_eq!(out["result"]["extraction"]["analysis"]["stale"], true);
    let get = commit(
        &db.state,
        &who,
        "document.review_get",
        json!({"document_id":doc}),
    )
    .await
    .unwrap();
    assert!(get["result"]["extraction_id"].is_null());
    assert_eq!(get["result"]["validation"]["ready"], false);
    db.drop().await;
}

#[tokio::test]
async fn v2_eval_scores_fields_nulls_repeated_rows_and_coverage() {
    let (fake, base) = fake_api().await;
    for _ in 0..4 {
        fake.answer(StatusCode::OK, openai_says(&rich_invoice()));
    }
    let extractor = Extractor::new(Config {
        provider: Provider::OpenAiCompatible,
        base_url: format!("{base}/v1"),
        api_key: None,
        model: "fake".into(),
    })
    .unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/invoices-v2");
    let score = payvy_server::eval::run(&extractor, &dir).await.unwrap();
    assert_eq!(score.failed_documents, 0);
    assert_eq!(score.total, 100);
    assert!(
        score.right >= 50 && score.right < 100,
        "only fractional and scanned match this deliberately fixed fake answer"
    );
}

#[tokio::test]
async fn revoked_reader_releases_claim_for_an_authorized_retry() {
    let Some((db, fake)) = db_with(Provider::OpenAiCompatible).await else {
        return;
    };
    let operator = person("operator");
    commit(
        &db.state,
        &operator,
        "person.add",
        json!({"name":"Clerk","actor":"clerk","email":"clerk@example.com","roles":["ap_clerk"]}),
    )
    .await
    .unwrap();
    let clerk = payvy_server::Actor::person(payvy_store::DEFAULT_ORG, "clerk");
    let doc = upload_rich(&db).await;
    let started = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *fake.gate.lock().unwrap() = Some((started.clone(), resume.clone()));
    fake.answer(StatusCode::OK, openai_says(&rich_invoice()));
    let state = db.state.clone();
    let d = doc.clone();
    let reading = tokio::spawn(async move {
        commit(&state, &clerk, "document.extract", json!({"document_id":d})).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    commit(
        &db.state,
        &operator,
        "person.set_roles",
        json!({"actor":"clerk","roles":["approver"]}),
    )
    .await
    .unwrap();
    resume.notify_one();
    assert!(matches!(
        reading.await.unwrap(),
        Err(CommandError::Forbidden(_))
    ));
    *fake.gate.lock().unwrap() = None;
    fake.answer(StatusCode::OK, openai_says(&rich_invoice()));
    let retry = commit(
        &db.state,
        &operator,
        "document.extract",
        json!({"document_id":doc}),
    )
    .await
    .unwrap();
    assert_eq!(retry["result"]["extraction"]["status"], "read");
    db.drop().await;
}

#[tokio::test]
async fn write_and_commit_failures_release_claim_without_clearing_a_replacement() {
    let Some((db, fake)) = db_with(Provider::OpenAiCompatible).await else {
        return;
    };
    let who = person("reviewer");
    let doc = upload_rich(&db).await;
    let id: uuid::Uuid = doc.parse().unwrap();
    for deferred in [false, true] {
        let mut tx = db.state.store.begin(who.org_id).await.unwrap();
        sqlx::query("CREATE OR REPLACE FUNCTION fail_extraction_write() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected extraction failure'; END $$").execute(&mut *tx).await.unwrap();
        let trigger = if deferred {
            "CREATE CONSTRAINT TRIGGER fail_extraction AFTER INSERT ON extractions DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION fail_extraction_write()"
        } else {
            "CREATE TRIGGER fail_extraction BEFORE INSERT ON extractions FOR EACH ROW EXECUTE FUNCTION fail_extraction_write()"
        };
        sqlx::query(trigger).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        fake.answer(StatusCode::OK, openai_says(&rich_invoice()));
        assert!(
            commit(
                &db.state,
                &who,
                "document.extract",
                json!({"document_id":doc})
            )
            .await
            .is_err()
        );
        let mut tx = db.state.store.begin(who.org_id).await.unwrap();
        let available: bool =
            sqlx::query_scalar("SELECT extraction_read_token IS NULL FROM documents WHERE id=$1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert!(available, "failed read must not wait for lease expiry");
        sqlx::query("DROP TRIGGER fail_extraction ON extractions")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    // A cleanup with an older token must not unlock a newer read.
    let mut tx = db.state.store.begin(who.org_id).await.unwrap();
    let replacement = uuid::Uuid::new_v4();
    assert!(
        payvy_store::claim_extraction_read(&mut tx, id, replacement, (db.state.clock)())
            .await
            .unwrap()
    );
    payvy_store::finish_extraction_read(&mut tx, id, uuid::Uuid::new_v4())
        .await
        .unwrap();
    assert!(
        payvy_store::extraction_read_is_current(&mut tx, id, replacement)
            .await
            .unwrap()
    );
    payvy_store::finish_extraction_read(&mut tx, id, replacement)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    fake.answer(StatusCode::OK, openai_says(&rich_invoice()));
    assert!(
        commit(
            &db.state,
            &who,
            "document.extract",
            json!({"document_id":doc})
        )
        .await
        .is_ok()
    );
    db.drop().await;
}

#[tokio::test]
async fn a_plan_without_the_reader_makes_no_model_calls() {
    let Some((mut db, fake)) = db_with(Provider::OpenAiCompatible).await else {
        return;
    };
    db.state = common::hosted(&db.state);
    let org = payvy_store::DEFAULT_ORG;
    // Bring your own agent: payments on, Payvy's reader off.
    common::entitle(&db.state, org, false, true, 1)
        .await
        .unwrap();
    let doc = upload(&db).await;

    // The background reader leaves the company alone; a direct read is refused.
    assert_eq!(
        payvy_server::extract_worker::tick(&db.state).await.unwrap(),
        0
    );
    for again in [false, true] {
        let e = commit(
            &db.state,
            &agent("claude-ap"),
            "document.extract",
            json!({ "document_id": doc, "again": again }),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, CommandError::NotEntitled(_)), "{e}");
        assert!(e.to_string().contains("your own agent"), "{e}");
    }
    assert!(fake.requests().is_empty());
    let usage = commit(&db.state, &person("robert"), "usage.summary", json!({}))
        .await
        .unwrap();
    assert_eq!(usage["result"]["attempts"], 0, "{usage:#}");

    // The customer's agent still files the bill from the document.
    let v = king(&db).await;
    let made = commit(
        &db.state,
        &agent("claude-ap"),
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "189727", "invoice_date": "2027-01-10",
                "amount_cents": 45_900, "document_id": doc }),
    )
    .await
    .unwrap();
    assert!(made["result"]["bill"]["id"].is_string(), "{made:#}");

    // Upgraded to the managed plan: the reader reads the next document.
    common::entitle(&db.state, org, true, true, 2)
        .await
        .unwrap();
    upload(&db).await;
    fake.answer(StatusCode::OK, openai_says(&king_invoice()));
    assert_eq!(
        payvy_server::extract_worker::tick(&db.state).await.unwrap(),
        1
    );
    assert_eq!(fake.requests().len(), 1);
    db.drop().await;
}
