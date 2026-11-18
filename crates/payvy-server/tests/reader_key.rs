mod common;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::Engine;
use common::*;
use payvy_extract::{Config, Extractor, Provider};
use payvy_server::{Bank, commands::CommandError, secrets::Sealer};
use payvy_store::DEFAULT_ORG;
use serde_json::{Value, json};

const GOOD: &str = "sk-ant-api03-company-key-for-tests-0000WXYZ";

/// An Anthropic stand-in: knows one key and one model, records which key
/// each read was billed to.
#[derive(Clone, Default)]
struct Fake {
    reads: Arc<Mutex<Vec<String>>>,
}

/// Anthropic's model list for a known key, or 401.
async fn models(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    match headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        Some(GOOD) => Ok(Json(json!({ "data": [
            { "id": "claude-sonnet-5-5" }, { "id": "claude-haiku-5-5" }
        ], "has_more": false }))),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// An OpenAI-compatible list as Gemini gives it: "models/" prefixes, and
/// models that can't read an invoice.
async fn gemini_models(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some("Bearer AIzaSy-gemini-test-key-000000000") => Ok(Json(json!({ "data": [
            { "id": "models/gemini-9-flash" }, { "id": "models/text-embedding-9" },
            { "id": "models/imagen-9" }
        ] }))),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn messages(State(fake): State<Fake>, headers: HeaderMap) -> Json<Value> {
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    fake.reads.lock().unwrap().push(key);
    let answer = json!({
        "kind": "invoice", "vendor_name": "Allied Storage Containers", "vendor_email": null,
        "vendor_address": null, "account_number": null, "invoice_number": "R261104319",
        "invoice_date": "2026-11-04", "due_date": null, "terms": null, "currency": "USD",
        "po_number": null, "subtotal": null, "tax": null, "total": "395.00", "lines": [], "notes": null
    });
    Json(
        json!({ "model": "claude-sonnet-5-5", "stop_reason": "end_turn",
                 "content": [{ "type": "text", "text": answer.to_string() }],
                 "usage": { "input_tokens": 10, "output_tokens": 10 } }),
    )
}

async fn start() -> (Fake, String) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/v1/models", get(models))
        .route("/models", get(gemini_models))
        .route("/v1/messages", post(messages))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, format!("http://{addr}"))
}

async fn upload(db: &TestDb, state: &payvy_server::AppState, name: &str) -> String {
    let pdf = format!("%PDF-1.7\n% {name}\n1 0 obj <<>> endobj\ntrailer <<>>\n%%EOF\n");
    let r = commit(
        state,
        &agent("email"),
        "document.upload",
        json!({ "filename": name,
                "content_base64": base64::engine::general_purpose::STANDARD.encode(pdf) }),
    )
    .await
    .unwrap();
    let _ = db;
    r["result"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_company_key_turns_on_reading_on_a_bring_your_own_agent_plan() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    let (fake, origin) = start().await;
    // Payvy's own reader exists (hosted always has one) but this plan is
    // bring-your-own-agent: reader off.
    let shared = Extractor::new(Config {
        provider: Provider::Anthropic,
        base_url: origin.clone(),
        api_key: Some("sk-ant-payvys-own-key".into()),
        model: "claude-sonnet-5-5".into(),
    })
    .unwrap();
    let mut state = hosted(
        &db.state
            .clone()
            .with_extractor(shared)
            .with_bank(Bank::new(Sealer::from_hex(&"5a".repeat(32)).unwrap())),
    );
    state.reader_key_origin = Some(origin.clone());
    db.state = state.clone();
    entitle(&state, DEFAULT_ORG, false, true, 1).await.unwrap();
    let admin = person("robert");

    let before = upload(&db, &state, "before.pdf").await;
    let backlog = upload(&db, &state, "backlog.pdf").await;
    let refused = commit(
        &state,
        &admin,
        "document.extract",
        json!({"document_id": before}),
    )
    .await;
    assert!(
        matches!(refused, Err(CommandError::NotEntitled(_))),
        "{refused:?}"
    );

    // A key the provider refuses, a key for the wrong provider, and a model
    // the key can't use are all refused before anything is stored.
    for (input, why) in [
        (
            json!({"provider":"anthropic","api_key":"sk-ant-api03-not-a-real-key-000000"}),
            "refused",
        ),
        (
            json!({"provider":"anthropic","api_key":"sk-proj-an-openai-key-000000000000"}),
            "isn't an Anthropic key",
        ),
        (
            json!({"provider":"anthropic","api_key":GOOD,"model":"claude-nonexistent"}),
            "can't use the model",
        ),
        (
            json!({"provider":"openai","api_key":"sk-proj-an-openai-key-000000000000"}),
            "choose the OpenAI model",
        ),
        (
            json!({"provider":"mistral","api_key":"sk-proj-an-openai-key-000000000000"}),
            "provider is one of",
        ),
    ] {
        match commit(&state, &admin, "reader_key.set", input).await {
            Err(CommandError::Invalid(m)) => assert!(m.contains(why), "{m}"),
            other => panic!("{why}: {other:?}"),
        }
    }
    assert_eq!(
        commit(&state, &admin, "reader_key.get", json!({}))
            .await
            .unwrap()["result"]["configured"],
        false
    );
    // An agent can't set one.
    assert!(matches!(
        commit(
            &state,
            &agent("helper"),
            "reader_key.set",
            json!({"provider":"anthropic","api_key":GOOD})
        )
        .await,
        Err(CommandError::PersonOnly(_))
    ));

    // The models each key can use, as its provider lists them.
    let listed = commit(
        &state,
        &admin,
        "reader_key.models",
        json!({"provider":"anthropic","api_key":GOOD}),
    )
    .await
    .unwrap();
    assert_eq!(
        listed["result"]["models"],
        json!(["claude-haiku-5-5", "claude-sonnet-5-5"])
    );
    assert_eq!(listed["result"]["suggested"], "claude-sonnet-5-5");
    let gemini = commit(
        &state,
        &admin,
        "reader_key.models",
        json!({"provider":"gemini","api_key":"AIzaSy-gemini-test-key-000000000"}),
    )
    .await
    .unwrap();
    assert_eq!(
        gemini["result"]["models"],
        json!(["gemini-9-flash"]),
        "prefix gone, non-readers left out"
    );
    assert!(
        fake.reads.lock().unwrap().is_empty(),
        "listing reads nothing"
    );

    db.advance(1);
    let set = commit(
        &state,
        &admin,
        "reader_key.set",
        json!({"provider":"anthropic","api_key":GOOD}),
    )
    .await
    .unwrap();
    assert_eq!(set["result"]["key_last4"], "WXYZ");
    assert_eq!(set["result"]["model"], "claude-sonnet-5-5");
    assert!(!set.to_string().contains(GOOD));
    let got = commit(&state, &admin, "reader_key.get", json!({}))
        .await
        .unwrap();
    assert!(!got.to_string().contains(GOOD));
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    let events = payvy_store::list_events(&mut tx, None, 100).await.unwrap();
    tx.rollback().await.unwrap();
    let events = serde_json::to_string(&events).unwrap();
    assert!(events.contains("reader_key.set") && !events.contains(GOOD));

    // The plan now shows the reader on.
    let org = commit(&state, &admin, "org.get", json!({})).await.unwrap();
    assert_eq!(org["result"]["plan"]["reader"], true);
    assert_eq!(org["result"]["plan"]["own_reader_key"], true);

    // Reading by hand works and is billed to the company's key.
    let read = commit(
        &state,
        &admin,
        "document.extract",
        json!({"document_id": before}),
    )
    .await
    .unwrap();
    // One read of the sample invoice when the key was checked, one now.
    assert_eq!(fake.reads.lock().unwrap().as_slice(), [GOOD, GOOD]);
    assert!(read.to_string().contains("company key"), "{read}");

    // The background reader reads what arrives after the key was set, not
    // the backlog.
    db.advance(1);
    let newer = upload(&db, &state, "newer.pdf").await;
    payvy_server::extract_worker::tick(&state).await.unwrap();
    let reads = fake.reads.lock().unwrap().len();
    assert!(reads >= 3, "the new invoice was read");
    let doc = commit(
        &state,
        &admin,
        "document.get",
        json!({"document_id": newer}),
    )
    .await
    .unwrap();
    assert!(
        !doc["result"]["extractions"].as_array().unwrap().is_empty(),
        "{doc}"
    );
    assert!(
        fake.reads.lock().unwrap().iter().all(|k| k == GOOD),
        "never Payvy's key"
    );
    assert_eq!(reads, 3, "only the new invoice, not the backlog");
    let left = commit(
        &state,
        &admin,
        "document.get",
        json!({"document_id": backlog}),
    )
    .await
    .unwrap();
    assert!(
        left["result"]["extractions"].as_array().unwrap().is_empty(),
        "an invoice from before the key was set isn't read on the company's bill: {left}"
    );

    // Removed: reading follows the plan again.
    commit(&state, &admin, "reader_key.remove", json!({}))
        .await
        .unwrap();
    let again = upload(&db, &state, "after-removal.pdf").await;
    assert!(matches!(
        commit(
            &state,
            &admin,
            "document.extract",
            json!({"document_id": again})
        )
        .await,
        Err(CommandError::NotEntitled(_))
    ));
    assert!(matches!(
        commit(&state, &admin, "reader_key.remove", json!({})).await,
        Err(CommandError::NotFound(_))
    ));
    db.drop().await;
}
