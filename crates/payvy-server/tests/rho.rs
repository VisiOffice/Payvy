//! Rho, the read-only bank, against a fake Rho that pages accounts and
//! transactions the way rhoapi.rho.co does. No real bank is called, and
//! Rho can never pay: with only Rho connected, a release is paid elsewhere
//! and marked paid, exactly as with no paying bank.

use axum::Json;
use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use payvy_server::Bank;
use payvy_server::secrets::Sealer;
use serde_json::{Value, json};
use std::collections::HashMap;

mod common;
use common::{TestDb, agent, commit, confirm, person, set_workflow, vendor};

const RHO_TOKEN: &str = "rhobat_test_good_token";
const MERCURY_TOKEN: &str = "secret-token:mercury_sandbox_good";

fn authed(h: &HeaderMap, token: &str) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {token}"))
}

fn problem(status: StatusCode, detail: &str) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(
            json!({ "type": "about:blank", "title": status.canonical_reason().unwrap_or("error"), "status": status.as_u16(), "detail": detail }),
        ),
    )
}

async fn rho_accounts(
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    if !authed(&h, RHO_TOKEN) {
        return problem(StatusCode::UNAUTHORIZED, "Token is revoked or has expired");
    }
    // Two pages: page one points at page two.
    if q.get("page_token").map(String::as_str) == Some("page-2") {
        return (
            StatusCode::OK,
            Json(json!({ "accounts": [
                { "id": "rho-acc-2", "account_name": "Savings", "account_type": "savings", "balance": { "amount": 100, "currency": "USD" }, "account_number_last_4": "9999" }
            ], "page": { "next_page_token": null } })),
        );
    }
    (
        StatusCode::OK,
        Json(json!({ "accounts": [
            { "id": "rho-acc-1", "account_name": "Operating", "account_type": "checking", "balance": { "amount": 5200050, "currency": "USD" }, "account_number_last_4": "6789" }
        ], "page": { "next_page_token": "page-2" } })),
    )
}

fn rho_txn(id: &str, cents: i64, counterparty: &str, memo: &str) -> Value {
    json!({ "id": id, "account_id": "rho-acc-1", "account_name": "Operating", "account_type": "checking",
            "transaction_type": "ach_debit", "status": "settled",
            "amount": { "amount": cents, "currency": "USD" }, "counterparty_name": counterparty,
            "initiated_at": "2026-11-01T09:00:00Z", "posted_at": "2026-11-03T09:00:00Z",
            "memo": memo, "money_movement_id": format!("mm-{id}") })
}

async fn rho_transactions(
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    if !authed(&h, RHO_TOKEN) {
        return problem(StatusCode::UNAUTHORIZED, "Token is revoked or has expired");
    }
    let all = vec![
        rho_txn("rho-txn-1", -245_000, "Northline Supply", "INV-1042"),
        rho_txn("rho-txn-2", -78_000, "Cedar Site Services", "INV-2086"),
        rho_txn("rho-txn-3", 500_000, "Customer Funding", "ELM-38"),
    ];
    let needle = q.get("search").map(|s| s.to_lowercase());
    let filtered: Vec<Value> = all
        .into_iter()
        .filter(|t| {
            needle.as_deref().is_none_or(|n| {
                t["counterparty_name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(n)
                    || t["memo"].as_str().unwrap_or("").to_lowercase().contains(n)
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(json!({ "transactions": filtered, "page": { "next_page_token": null } })),
    )
}

async fn mercury_accounts(h: HeaderMap) -> (StatusCode, Json<Value>) {
    if !authed(&h, MERCURY_TOKEN) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "errors": { "message": "invalid token" } })),
        );
    }
    (
        StatusCode::OK,
        Json(json!({ "accounts": [
            { "id": "acc-checking", "name": "Ops Checking", "status": "active", "availableBalance": 52000.5, "canSendRealTimePayments": true }
        ] })),
    )
}

async fn spawn(routes: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    format!("http://{addr}")
}

async fn db() -> Option<TestDb> {
    let mut db = TestDb::new().await?;
    let rho_base = spawn(
        axum::Router::new()
            .route("/accounts", get(rho_accounts))
            .route("/transactions", get(rho_transactions)),
    )
    .await;
    let mercury_base = spawn(axum::Router::new().route("/accounts", get(mercury_accounts))).await;
    db.state = db.state.clone().with_bank(Bank {
        sealer: Sealer::from_hex(&"5a".repeat(32)).unwrap(),
        mercury_production: mercury_base.clone(),
        mercury_sandbox: mercury_base,
        rho_production: rho_base.clone(),
        rho_sandbox: rho_base,
    });
    Some(db)
}

async fn connect_rho(db: &TestDb) -> Value {
    commit(
        &db.state,
        &person("robert"),
        "bank.connect",
        json!({ "provider": "rho", "token": RHO_TOKEN, "environment": "sandbox" }),
    )
    .await
    .unwrap()["result"]
        .clone()
}

#[tokio::test]
async fn rho_connects_reads_and_never_leaks_the_token() {
    let Some(db) = db().await else { return };
    // Agents prepare work; connecting a bank is a person's job.
    let agent_try = commit(
        &db.state,
        &agent("claude-ap"),
        "bank.connect",
        json!({ "provider": "rho", "token": RHO_TOKEN, "environment": "sandbox" }),
    )
    .await;
    assert!(agent_try.is_err(), "agent must not connect a bank");

    let connected = connect_rho(&db).await;
    assert_eq!(connected["connection"]["provider"], "rho");
    assert_eq!(connected["connection"]["mode"], "read_only");
    assert_eq!(connected["capability"], "read_only");
    // Several accounts, no account_id: the result names the pick.
    assert_eq!(connected["display_account"]["id"], "rho-acc-1");
    assert!(connected["note"].as_str().unwrap().contains("first of 2"));
    assert!(
        !serde_json::to_string(&connected)
            .unwrap()
            .contains(RHO_TOKEN),
        "the token must never appear in a result"
    );

    // An agent key can read accounts and transactions.
    let accounts = commit(&db.state, &agent("claude-ap"), "bank.accounts", json!({}))
        .await
        .unwrap()["result"]
        .clone();
    let list = accounts["accounts"].as_array().unwrap();
    assert_eq!(list.len(), 2, "both Rho pages are followed");
    assert_eq!(list[0]["available_cents"], 5_200_050);
    assert!(
        !serde_json::to_string(&accounts).unwrap().contains("6789"),
        "account numbers beyond the bank's own fields never surface from Payvy"
    );

    let txns = commit(
        &db.state,
        &agent("claude-ap"),
        "bank.transactions",
        json!({ "search": "northline" }),
    )
    .await
    .unwrap()["result"]
        .clone();
    let rows = txns["transactions"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["amount_cents"], -245_000);
    assert_eq!(rows[0]["counterparty_name"], "Northline Supply");

    let limited = commit(
        &db.state,
        &person("robert"),
        "bank.transactions",
        json!({ "limit": 2 }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(limited["transactions"].as_array().unwrap().len(), 2);

    // bank.get: no paying bank, but the read-only connection is listed.
    let got = commit(&db.state, &person("robert"), "bank.get", json!({}))
        .await
        .unwrap()["result"]
        .clone();
    assert!(got["connection"].is_null());
    assert_eq!(got["connections"].as_array().unwrap().len(), 1);
    assert_eq!(got["connections"][0]["capability"], "read_only");
    db.drop().await;
}

#[tokio::test]
async fn rho_coexists_with_mercury_and_cannot_pay() {
    let Some(db) = db().await else { return };
    let robert = person("robert");
    commit(
        &db.state,
        &robert,
        "bank.connect",
        json!({ "provider": "mercury", "token": MERCURY_TOKEN, "mode": "request", "account_id": "acc-checking" }),
    )
    .await
    .unwrap();
    connect_rho(&db).await;

    let got = commit(&db.state, &robert, "bank.get", json!({}))
        .await
        .unwrap()["result"]
        .clone();
    assert_eq!(
        got["connection"]["provider"], "mercury",
        "paying bank survives a Rho connect"
    );
    assert_eq!(got["connections"].as_array().unwrap().len(), 2);

    // Disconnect Mercury; Rho remains, and release refuses read-only.
    commit(&db.state, &robert, "bank.disconnect", json!({}))
        .await
        .unwrap();
    let v = vendor(&db.state, &robert, "King Environmental").await;
    set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let bill_id = common::bill(&db.state, &agent("claude-ap"), &v, "189727", 45_900).await;
    // With only Rho connected, release behaves as with no paying bank:
    // Payvy won't send it; the bill is paid elsewhere and marked paid.
    // (Refusing here dead-ended the web app's Pay dialog.)
    let out = confirm(
        &db.state,
        &robert,
        "bill.release",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap();
    assert_eq!(out["result"]["progress"]["status"], "released");
    assert!(
        out["result"]["next"]
            .as_str()
            .unwrap()
            .contains("bill.mark_paid"),
        "release must point at bill.mark_paid, got: {out}"
    );
    let paid = confirm(
        &db.state,
        &robert,
        "bill.mark_paid",
        json!({ "bill_id": bill_id, "paid_on": "2027-01-20", "method": "ach" }),
    )
    .await
    .unwrap();
    assert_eq!(paid["result"]["bill"]["state"], "paid");

    // Disconnecting Rho by provider works and leaves nothing behind.
    let gone = commit(
        &db.state,
        &robert,
        "bank.disconnect",
        json!({ "provider": "rho" }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(gone["provider"], "rho");
    let got = commit(&db.state, &robert, "bank.get", json!({}))
        .await
        .unwrap()["result"]
        .clone();
    assert_eq!(got["connections"].as_array().unwrap().len(), 0);
    db.drop().await;
}

#[tokio::test]
async fn rho_bad_token_is_a_clear_refusal() {
    let Some(db) = db().await else { return };
    let err = commit(
        &db.state,
        &person("robert"),
        "bank.connect",
        json!({ "provider": "rho", "token": "rhobat_wrong", "environment": "sandbox" }),
    )
    .await
    .expect_err("a bad Rho token must not connect");
    assert!(
        err.to_string().contains("Rho refused this token"),
        "got: {err}"
    );
    db.drop().await;
}
