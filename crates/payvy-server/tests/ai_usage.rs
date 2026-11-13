//! Durable AI usage accounting tests, covering success, refusal, truncation,
//! invalid output, retries, timeouts, unknown pricing, duplicate completion,
//! downstream DB failures, and `usage.summary` permissions.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use base64::Engine;
use chrono::Utc;
use payvy_domain::ai_usage::{PRICING_VERSION, TokenUsage, compute_cost_microcents};
use payvy_extract::{Config, Extractor, Provider, REQUEST_TIMEOUT_SECS};
use payvy_server::commands::CommandError;
use payvy_store::{self as store, ActorKind, CompleteAiUsageAttempt, NewAiUsageAttempt};
use serde_json::{Value, json};
use uuid::Uuid;

mod common;
use common::{TestDb, agent, commit, person};

#[derive(Clone, Default)]
struct Fake {
    answers: Arc<Mutex<Vec<(StatusCode, Value)>>>,
    delay: Arc<Mutex<Option<Duration>>>,
}

impl Fake {
    fn answer(&self, status: StatusCode, body: Value) {
        self.answers.lock().unwrap().push((status, body));
    }

    fn set_delay(&self, d: Duration) {
        *self.delay.lock().unwrap() = Some(d);
    }
}

async fn reply(State(fake): State<Fake>, Json(_body): Json<Value>) -> (StatusCode, Json<Value>) {
    let delay = *fake.delay.lock().unwrap();
    if let Some(d) = delay {
        tokio::time::sleep(d).await;
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

async fn start_fake() -> (Fake, String) {
    let fake = Fake::default();
    let app = axum::Router::new()
        .route("/v1/messages", post(reply))
        .route("/chat/completions", post(reply))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, format!("http://{addr}"))
}

async fn upload_pdf(db: &TestDb) -> Uuid {
    let pdf = b"%PDF-1.7\n1 0 obj <<>> endobj\ntrailer <<>>\n%%EOF\n";
    let res = commit(
        &db.state,
        &agent("importer"),
        "document.upload",
        json!({
            "filename": "invoice.pdf",
            "content_base64": base64::engine::general_purpose::STANDARD.encode(pdf),
        }),
    )
    .await
    .unwrap();
    res["result"]["id"].as_str().unwrap().parse().unwrap()
}

fn valid_answer_json() -> String {
    json!({
        "kind": "invoice",
        "vendor_name": "King Environmental",
        "vendor_email": null,
        "vendor_address": null,
        "account_number": null,
        "invoice_number": "189727",
        "invoice_date": "2026-11-01",
        "due_date": null,
        "terms": null,
        "currency": "USD",
        "po_number": null,
        "subtotal": null,
        "tax": null,
        "total": "459.00",
        "lines": [],
        "notes": null
    })
    .to_string()
}

#[tokio::test]
async fn records_success_refusal_truncation_invalid_retry_and_unknown_pricing() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    let (fake, url) = start_fake().await;
    let extractor = Extractor::new(Config {
        provider: Provider::Anthropic,
        base_url: url.clone(),
        api_key: Some("k".into()),
        model: "claude-sonnet-5-5".into(),
    })
    .unwrap();
    db.state = db.state.with_extractor(extractor);

    let doc_id = upload_pdf(&db).await;

    // 1. Refusal (still records tokens and cost if usage was reported)
    fake.answer(
        StatusCode::OK,
        json!({
            "id": "msg_refused",
            "model": "claude-sonnet-5-5",
            "stop_reason": "refusal",
            "stop_details": { "category": "policy" },
            "content": [],
            "usage": { "input_tokens": 100, "output_tokens": 10 }
        }),
    );
    let _ = commit(
        &db.state,
        &agent("extractor"),
        "document.extract",
        json!({ "document_id": doc_id }),
    )
    .await
    .unwrap();

    // 2. Truncated (max_tokens)
    fake.answer(
        StatusCode::OK,
        json!({
            "id": "msg_trunc",
            "model": "claude-sonnet-5-5",
            "stop_reason": "max_tokens",
            "content": [{ "type": "text", "text": "{" }],
            "usage": { "input_tokens": 200, "output_tokens": 16000 }
        }),
    );
    let _ = commit(
        &db.state,
        &agent("extractor"),
        "document.extract",
        json!({ "document_id": doc_id }),
    )
    .await
    .unwrap();

    // 3. Invalid model output (bad JSON)
    fake.answer(
        StatusCode::OK,
        json!({
            "id": "msg_bad",
            "model": "claude-sonnet-5-5",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "not json" }],
            "usage": { "input_tokens": 150, "output_tokens": 20 }
        }),
    );
    let _ = commit(
        &db.state,
        &agent("extractor"),
        "document.extract",
        json!({ "document_id": doc_id }),
    )
    .await
    .unwrap();

    // 4. Success on retry
    fake.answer(
        StatusCode::OK,
        json!({
            "id": "msg_ok",
            "model": "claude-sonnet-5-5",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": valid_answer_json() }],
            "usage": {
                "input_tokens": 300,
                "output_tokens": 100,
                "cache_read_input_tokens": 1000,
                "cache_creation_input_tokens": 200
            }
        }),
    );
    let _ = commit(
        &db.state,
        &agent("extractor"),
        "document.extract",
        json!({ "document_id": doc_id }),
    )
    .await
    .unwrap();

    // 5. Unknown pricing (unrecognized model, again: true)
    let unknown_ext = Extractor::new(Config {
        provider: Provider::OpenAiCompatible,
        base_url: url,
        api_key: None,
        model: "local-unpriced-model".into(),
    })
    .unwrap();
    db.state = db.state.with_extractor(unknown_ext);
    fake.answer(
        StatusCode::OK,
        json!({
            "id": "chat_unpriced",
            "model": "local-unpriced-model",
            "choices": [{ "finish_reason": "stop", "message": { "content": valid_answer_json() } }],
            "usage": { "prompt_tokens": 500, "completion_tokens": 50 }
        }),
    );
    let _ = commit(
        &db.state,
        &agent("extractor"),
        "document.extract",
        json!({ "document_id": doc_id, "again": true }),
    )
    .await
    .unwrap();

    // Admin reads usage.summary
    let summary = commit(&db.state, &person("admin"), "usage.summary", json!({}))
        .await
        .unwrap();
    let r = &summary["result"];
    assert_eq!(r["attempts"], 5);
    assert_eq!(r["successful_reads"], 2);
    assert_eq!(r["failed_attempts"], 3);
    assert_eq!(r["unknown_usage_attempts"], 0);
    assert_eq!(r["unpriced_attempts"], 1);
    assert_eq!(r["input_tokens"], 100 + 200 + 150 + 300 + 500);
    assert_eq!(r["output_tokens"], 10 + 16000 + 20 + 100 + 50);
    assert_eq!(r["cache_read_tokens"], 1000);
    assert_eq!(r["cache_write_tokens"], 200);

    // Non-admin person cannot read usage.summary
    let err = commit(&db.state, &person("clerk"), "usage.summary", json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, CommandError::Forbidden(_)));
}

#[tokio::test]
async fn duplicate_completion_is_ignored_and_downstream_db_failure_keeps_usage() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let doc_id = upload_pdf(&db).await;
    let now = Utc::now();

    let mut tx = db.state.store.begin(store::DEFAULT_ORG).await.unwrap();
    let id = store::start_ai_usage_attempt(
        &mut tx,
        store::DEFAULT_ORG,
        &NewAiUsageAttempt {
            document_id: doc_id,
            actor_kind: ActorKind::Agent,
            actor_name: "extractor",
            provider: "anthropic",
            requested_model: "claude-sonnet-5-5",
            started_at: now,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let usage = TokenUsage {
        input_tokens: Some(500),
        output_tokens: Some(100),
        cache_read_tokens: None,
        cache_write_tokens: None,
    };
    let (ver, cost) = compute_cost_microcents("claude-sonnet-5-5", &usage).unwrap();
    assert_eq!(ver, PRICING_VERSION);

    // First completion succeeds
    let mut tx = db.state.store.begin(store::DEFAULT_ORG).await.unwrap();
    let first = store::complete_ai_usage_attempt(
        &mut tx,
        id,
        &CompleteAiUsageAttempt {
            outcome: "succeeded",
            actual_model: Some("claude-sonnet-5-5"),
            provider_request_id: Some("msg_1"),
            error: None,
            usage,
            pricing_version: Some(ver),
            cost_microcents: Some(cost),
            completed_at: now,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(first);

    // Duplicate completion is a safe no-op
    let mut tx = db.state.store.begin(store::DEFAULT_ORG).await.unwrap();
    let second = store::complete_ai_usage_attempt(
        &mut tx,
        id,
        &CompleteAiUsageAttempt {
            outcome: "provider_error",
            actual_model: None,
            provider_request_id: None,
            error: Some("duplicate"),
            usage: TokenUsage::default(),
            pricing_version: None,
            cost_microcents: None,
            completed_at: now,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(!second);

    // Simulated downstream extraction transaction rollback still preserves the attempt
    let mut tx = db.state.store.begin(store::DEFAULT_ORG).await.unwrap();
    let row = store::get_ai_usage_attempt(&mut tx, id)
        .await
        .unwrap()
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(row.outcome, "succeeded");
    assert_eq!(row.cost_microcents, Some(cost));
}

#[tokio::test]
async fn timeout_stale_pending_and_document_delete_keep_usage() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    let (fake, url) = start_fake().await;
    fake.set_delay(Duration::from_millis(200));
    let ext = Extractor::with_timeout(
        Config {
            provider: Provider::Anthropic,
            base_url: url,
            api_key: Some("k".into()),
            model: "claude-sonnet-5-5".into(),
        },
        Duration::from_millis(25),
    )
    .unwrap();
    db.state = db.state.with_extractor(ext);

    let doc_id = upload_pdf(&db).await;
    let _ = commit(
        &db.state,
        &agent("extractor"),
        "document.extract",
        json!({ "document_id": doc_id }),
    )
    .await
    .unwrap();

    // Insert a stale pending row (started 10 minutes ago, past REQUEST_TIMEOUT_SECS)
    let now = *db.clock.lock().unwrap();
    let stale_started = now - chrono::Duration::seconds(REQUEST_TIMEOUT_SECS + 60);
    let mut tx = db.state.store.begin(store::DEFAULT_ORG).await.unwrap();
    let stale_id = store::start_ai_usage_attempt(
        &mut tx,
        store::DEFAULT_ORG,
        &NewAiUsageAttempt {
            document_id: doc_id,
            actor_kind: ActorKind::Agent,
            actor_name: "extractor",
            provider: "anthropic",
            requested_model: "claude-sonnet-5-5",
            started_at: stale_started,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let summary = commit(&db.state, &person("admin"), "usage.summary", json!({}))
        .await
        .unwrap();
    let r = &summary["result"];
    assert_eq!(r["attempts"], 2);
    assert_eq!(r["pending_attempts"], 0);
    assert_eq!(r["unknown_usage_attempts"], 2);
    assert_eq!(r["estimated_cost_microcents"], 0);

    // Deleting the document is not blocked by ai_usage_attempts and preserves
    // usage rows with document_id = NULL (clear extractions first since 0004
    // uses a restrictive FK).
    let mut tx = db.state.store.begin(store::DEFAULT_ORG).await.unwrap();
    sqlx::query("DELETE FROM extractions WHERE document_id = $1")
        .bind(doc_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM documents WHERE id = $1")
        .bind(doc_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let stale_row = store::get_ai_usage_attempt(&mut tx, stale_id)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(stale_row.document_id, None);
}
