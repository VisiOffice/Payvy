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
#[derive(Clone, Default)]
struct Fake {
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
    assert!(
        first["error"]
            .as_str()
            .unwrap()
            .contains("model is loading"),
        "{first}"
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
    assert_eq!(score.total, 9 + 9 + 7 + 3 + 4);
    assert!(
        score.right >= 9 && score.right < score.total,
        "{}",
        score.right
    );
}
