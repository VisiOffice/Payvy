//! Phase 1 against a mock VisiAPI over real HTTP: connecting through
//! Settings, posting through the real VisiBooks adapter, an outage and
//! its retry, a closed period, and the payment leg's retry.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::any;
use payvy_server::auth::Auth;
use payvy_server::mailer::Mailer;
use payvy_server::{Bank, secrets::Sealer};
use payvy_store::{self as store, DEFAULT_ORG, NewLedgerRecord};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

mod common;
use common::{TestDb, commit, confirm, person};

#[derive(Clone, Default)]
struct Mock {
    calls: Arc<Mutex<Vec<(String, String, Value)>>>,
    fail_bills: Arc<Mutex<bool>>,
    fail_payments: Arc<Mutex<bool>>,
    closed_period: Arc<Mutex<bool>>,
}

async fn handle(
    State(m): State<Mock>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let path = uri.path().to_owned();
    // Recover the kind from the body shape: bills carry "bill", payments
    // carry "payment".
    let kind = if body.get("bill").is_some() {
        "bill"
    } else if body.get("payment").is_some() {
        "payment"
    } else if body.get("document_link").is_some() {
        "document_link"
    } else if body.get("bank_reference").is_some() {
        "bank_reference"
    } else {
        "other"
    };
    assert_eq!(headers["x-books-entity-id"], "12");
    m.calls
        .lock()
        .unwrap()
        .push((kind.into(), path.clone(), body));
    if kind == "bill" && *m.closed_period.lock().unwrap() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": { "code": "PERIOD_CLOSED" } })),
        );
    }
    if (kind == "bill" && *m.fail_bills.lock().unwrap())
        || (kind == "payment" && *m.fail_payments.lock().unwrap())
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": { "code": "BOOM" } })),
        );
    }
    let id = if kind == "payment" { 99 } else { 88 };
    // The adapter checks the echoed external key against the one in the
    // URL's last segment (or the one after by_external/payvy/).
    let echoed = path
        .split("/payvy/")
        .nth(1)
        .map(|rest| rest.split('/').next().unwrap_or_default().to_owned())
        .unwrap_or_default();
    let echoed = if echoed.is_empty() || echoed == "bills" || echoed == "vendor_payments" {
        "payvy:bill:x".to_owned()
    } else {
        echoed
    };
    (
        StatusCode::OK,
        Json(json!({ "data": { "id": id, "external_id": echoed, "status": "posted" } })),
    )
}

struct Fx {
    db: TestDb,
    state: payvy_server::AppState,
    mock: Mock,
    vendor: Uuid,
}

async fn serve(mock: Mock) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new().fallback(any(handle)).with_state(mock);
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (origin, task)
}

async fn setup(db: TestDb, origin: &str, post_when: &str) -> Fx {
    let sealer = Sealer::from_hex(&"5a".repeat(32)).unwrap();
    let state = db
        .state
        .clone()
        .with_bank(Bank {
            sealer,
            ramp: None,
            mercury_production: "https://mercury.invalid".into(),
            mercury_sandbox: "http://127.0.0.1:9".into(),
            rho_production: "https://rho.invalid".into(),
            rho_sandbox: "http://127.0.0.1:9".into(),
            webhook_base: None,
        })
        .with_auth(Auth::new(
            "https://app.payvy.example",
            Mailer::Memory(Arc::new(Mutex::new(Vec::new()))),
        ));
    confirm(
        &state,
        &person("ada"),
        "ledger.connect",
        json!({ "origin": origin, "entity": "12", "api_key": "vk_test", "post_when": post_when }),
    )
    .await
    .unwrap();
    confirm(
        &state,
        &person("ada"),
        "ledger.configure",
        json!({ "ap_account_id": "25", "defaults": { "expense_account_id": "65" } }),
    )
    .await
    .unwrap();
    let vendor = commit(
        &state,
        &person("ada"),
        "vendor.create",
        json!({ "name": "Acme" }),
    )
    .await
    .unwrap()["result"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let vendor_id = Uuid::parse_str(&vendor).unwrap();
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    let conn = store::active_ledger_connection(&mut tx)
        .await
        .unwrap()
        .unwrap();
    store::put_ledger_link(
        &mut tx,
        DEFAULT_ORG,
        &store::PutLedgerLink {
            connection_id: conn.id,
            entity: "vendor",
            payvy_id: vendor_id,
            external_id: Some("77"),
            external_key: "vendor",
            status: "linked",
            error_code: None,
            error_detail: None,
            request_hash: None,
            posted_at: None,
        },
    )
    .await
    .unwrap();
    store::insert_ledger_record(
        &mut tx,
        DEFAULT_ORG,
        &NewLedgerRecord {
            connection_id: conn.id,
            kind: "account",
            external_id: "65",
            name: "Repairs",
            parent_external_id: None,
            inactive: false,
            detail: &json!({ "type": "expense" }),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    Fx {
        db,
        state,
        mock: Mock::default(),
        vendor: vendor_id,
    }
}

async fn invoice(fx: &Fx, number: &str) -> Uuid {
    let created = commit(
        &fx.state,
        &person("ada"),
        "bill.create",
        json!({ "vendor_id": fx.vendor, "invoice_number": number,
                "invoice_date": "2027-01-15", "amount_cents": 10_000 }),
    )
    .await
    .unwrap();
    Uuid::parse_str(created["result"]["bill"]["id"].as_str().unwrap()).unwrap()
}

async fn force_due(fx: &Fx) {
    let mut tx = fx.state.store.begin(DEFAULT_ORG).await.unwrap();
    sqlx::query("UPDATE ledger_jobs SET next_attempt_at = '2000-01-01T00:00:00Z'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

fn kinds(fx: &Fx) -> Vec<String> {
    fx.mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(k, _, _)| k.clone())
        .collect()
}

#[tokio::test]
async fn connect_posts_through_the_real_adapter_and_rejects_private_origins() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mock = Mock::default();
    let (origin, task) = serve(mock.clone()).await;
    let fx = setup(db, &origin, "approval").await;
    // A private address is refused before anything is stored or called.
    let e = confirm(
        &fx.state,
        &person("ada"),
        "ledger.connect",
        json!({ "origin": "https://192.168.1.20", "entity": "12", "api_key": "vk_test" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("private"), "{e}");

    common::set_workflow(
        &fx.state,
        &person("ada"),
        Some(&fx.vendor.to_string()),
        json!([
            { "id": "ok", "type": "approve", "approvers": ["ada"] },
            { "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let fx = Fx { mock, ..fx };
    let bill = invoice(&fx, "VB-1").await;
    commit(
        &fx.state,
        &person("ada"),
        "bill.approve",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    assert!(fx.mock.calls.lock().unwrap().is_empty(), "queued only");
    payvy_server::ledger_worker::tick(&fx.state).await.unwrap();
    assert_eq!(kinds(&fx), ["bill", "document_link"]);
    let body = fx.mock.calls.lock().unwrap()[0].2.clone();
    assert_eq!(body["bill"]["vendor_invoice_number"], "VB-1");
    task.abort();
    fx.db.drop().await;
}

#[tokio::test]
async fn an_outage_retries_and_a_closed_period_lands_on_the_link() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mock = Mock::default();
    // Nothing listening yet: the origin is a closed port.
    let (origin, task) = serve(Mock::default()).await;
    task.abort();
    let fx = setup(db, &origin, "approval").await;
    let fx = Fx {
        mock: mock.clone(),
        ..fx
    };
    common::set_workflow(
        &fx.state,
        &person("ada"),
        Some(&fx.vendor.to_string()),
        json!([
            { "id": "ok", "type": "approve", "approvers": ["ada"] },
            { "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let bill = invoice(&fx, "VB-2").await;
    commit(
        &fx.state,
        &person("ada"),
        "bill.approve",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    payvy_server::ledger_worker::tick(&fx.state).await.unwrap();
    // Failed and recorded; then the books come back and the retry posts.
    let (_origin2, task2) = serve_at(&fx, &origin).await;
    force_due(&fx).await;
    payvy_server::ledger_worker::tick(&fx.state).await.unwrap();
    assert_eq!(kinds(&fx), ["bill", "document_link"]);

    // A closed period is a recorded, named failure on the link.
    *fx.mock.closed_period.lock().unwrap() = true;
    let bill2 = invoice(&fx, "VB-3").await;
    commit(
        &fx.state,
        &person("ada"),
        "bill.approve",
        json!({ "bill_id": bill2 }),
    )
    .await
    .unwrap();
    payvy_server::ledger_worker::tick(&fx.state).await.unwrap();
    let mut tx = fx.state.store.begin(DEFAULT_ORG).await.unwrap();
    let conn = store::active_ledger_connection(&mut tx)
        .await
        .unwrap()
        .unwrap();
    let link = store::ledger_link(&mut tx, conn.id, "bill", bill2)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(link.status, "error");
    assert_eq!(link.error_code.as_deref(), Some("period_closed"));
    tx.rollback().await.unwrap();
    task2.abort();
    fx.db.drop().await;
}

async fn serve_at(fx: &Fx, origin: &str) -> (String, tokio::task::JoinHandle<()>) {
    let port: u16 = origin.rsplit(':').next().unwrap().parse().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let app = axum::Router::new()
        .fallback(any(handle))
        .with_state(fx.mock.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (origin.to_owned(), task)
}

#[tokio::test]
async fn a_failed_payment_leg_retries_only_the_payment_and_sends_its_reference() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mock = Mock::default();
    let (origin, task) = serve(mock.clone()).await;
    let fx = setup(db, &origin, "payment").await;
    let fx = Fx { mock, ..fx };
    // Map the outside method, then pay outside with a reference.
    confirm(
        &fx.state,
        &person("ada"),
        "ledger.configure",
        json!({ "bank_account_map": { "ach": "900" } }),
    )
    .await
    .unwrap();
    common::set_workflow(
        &fx.state,
        &person("ada"),
        Some(&fx.vendor.to_string()),
        json!([{ "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let bill = invoice(&fx, "VB-4").await;
    confirm(
        &fx.state,
        &person("ada"),
        "bill.release",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    *fx.mock.fail_payments.lock().unwrap() = true;
    confirm(
        &fx.state,
        &person("ada"),
        "bill.mark_paid",
        json!({ "bill_id": bill, "paid_on": "2027-01-20", "method": "ach", "reference": "ach-77" }),
    )
    .await
    .unwrap();
    payvy_server::ledger_worker::tick(&fx.state).await.unwrap();
    assert_eq!(kinds(&fx), ["bill", "document_link", "payment"]);
    // The retry posts only the payment, then sends its bank reference.
    *fx.mock.fail_payments.lock().unwrap() = false;
    force_due(&fx).await;
    payvy_server::ledger_worker::tick(&fx.state).await.unwrap();
    assert_eq!(
        kinds(&fx),
        [
            "bill",
            "document_link",
            "payment",
            "payment",
            "bank_reference"
        ]
    );
    // And a second drain with nothing changed sends nothing more.
    payvy_server::ledger_worker::tick(&fx.state).await.unwrap();
    assert_eq!(kinds(&fx).len(), 5);
    task.abort();
    fx.db.drop().await;
}
