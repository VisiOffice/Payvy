//! Paying through the customer's bank, against a real database and a fake
//! Mercury that keeps idempotency keys, approval requests and transaction
//! statuses the way the real API does. No real bank is called.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use payvy_server::commands::CommandError;
use payvy_server::secrets::Sealer;
use payvy_server::{Bank, payment_worker};
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, commit, confirm, person, set_workflow, status, vendor};

const TOKEN: &str = "secret-token:mercury_sandbox_good";
const CHECKING: &str = "acc-checking";
const KING: &str = "rcp-king";

#[derive(Default)]
struct Bank0 {
    /// idempotency key -> transaction or request id
    keys: HashMap<String, String>,
    transactions: HashMap<String, Value>,
    requests: HashMap<String, Value>,
    /// The next payment gets this answer instead: (status, message, still create it).
    next: Option<(u16, &'static str, bool)>,
    posts: Vec<Value>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Bank0>>);

type Reply = (StatusCode, Json<Value>);

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn unauthorized() -> Reply {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "errors": { "message": "invalid token" } })),
    )
}

async fn accounts(h: HeaderMap) -> Reply {
    if !authed(&h) {
        return unauthorized();
    }
    (
        StatusCode::OK,
        Json(json!({ "accounts": [
            { "id": CHECKING, "name": "Ops Checking ••1234", "status": "active", "type": "mercury", "kind": "checking", "availableBalance": 52000.5, "canSendRealTimePayments": true },
            { "id": "acc-savings", "name": "Savings", "status": "active", "type": "mercury", "kind": "savings", "availableBalance": 1.0, "canSendRealTimePayments": false },
            { "id": "acc-old", "name": "Old", "status": "archived", "type": "mercury", "kind": "checking", "availableBalance": 0, "canSendRealTimePayments": false }
        ] })),
    )
}

fn king() -> Value {
    json!({ "id": KING, "name": "King Environmental", "status": "active", "emails": ["ar@kingenv.com"],
            "electronicRoutingInfo": { "accountNumber": "000123456789", "routingNumber": "121000248", "electronicAccountType": "businessChecking" },
            "checkInfo": { "address": { "address1": "PO Box 4410", "city": "Mesa", "region": "AZ", "postalCode": "85201", "country": "US" } } })
}

async fn recipients(h: HeaderMap) -> Reply {
    if !authed(&h) {
        return unauthorized();
    }
    (
        StatusCode::OK,
        Json(json!({ "recipients": [king()], "page": {} })),
    )
}

async fn recipient(h: HeaderMap, Path(id): Path<String>) -> Reply {
    if !authed(&h) {
        return unauthorized();
    }
    if id == KING {
        (StatusCode::OK, Json(king()))
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "errors": { "message": "not found" } })),
        )
    }
}

impl Fake {
    fn pay(&self, account: &str, body: Value, request: bool) -> Reply {
        let mut b = self.0.lock().unwrap();
        b.posts.push(body.clone());
        let key = body["idempotencyKey"].as_str().unwrap().to_owned();
        if let Some(id) = b.keys.get(&key) {
            let existing = b
                .transactions
                .get(id)
                .or(b.requests.get(id))
                .unwrap()
                .clone();
            return (StatusCode::CONFLICT, Json(existing));
        }
        let next = b.next.take();
        if let Some((status, message, false)) = next {
            return (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({ "errors": { "message": message } })),
            );
        }
        let id = format!(
            "{}-{}",
            if request { "req" } else { "txn" },
            b.keys.len() + 1
        );
        let made = if request {
            json!({ "requestId": id, "accountId": account, "recipientId": body["recipientId"],
                    "paymentMethod": body["paymentMethod"], "amount": body["amount"],
                    "status": "pendingApproval", "reviews": [], "requestedByUserId": "u1", "createdAt": "2027-01-15T00:00:00Z" })
        } else {
            json!({ "id": id, "status": "pending", "amount": body["amount"], "counterpartyId": body["recipientId"],
                    "counterpartyName": "King Environmental", "dashboardLink": format!("https://app.mercury.com/transactions/{id}"),
                    "createdAt": "2027-01-15T00:00:00Z", "estimatedDeliveryDate": "2027-01-17T00:00:00Z" })
        };
        b.keys.insert(key, id.clone());
        if request {
            b.requests.insert(id, made.clone());
        } else {
            b.transactions.insert(id, made.clone());
        }
        match next {
            // Made it, but the answer was lost.
            Some((status, message, true)) => (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({ "errors": { "message": message } })),
            ),
            _ => (StatusCode::OK, Json(made)),
        }
    }

    fn set_transaction(&self, id: &str, status: &str) {
        let mut b = self.0.lock().unwrap();
        b.transactions.get_mut(id).unwrap()["status"] = json!(status);
        if status == "failed" {
            b.transactions.get_mut(id).unwrap()["reasonForFailure"] = json!("account closed");
        }
    }

    /// A Mercury admin approves the request; Mercury makes the transaction.
    fn approve(&self, request_id: &str) -> String {
        let mut b = self.0.lock().unwrap();
        let r = b.requests.get_mut(request_id).unwrap();
        r["status"] = json!("approved");
        let amount = r["amount"].clone();
        let id = format!("txn-for-{request_id}");
        b.transactions.insert(
            id.clone(),
            json!({ "id": id, "status": "pending", "amount": amount, "requestId": request_id,
                    "counterpartyId": KING, "counterpartyName": "King Environmental",
                    "createdAt": "2027-01-15T00:00:00Z", "estimatedDeliveryDate": "2027-01-17T00:00:00Z" }),
        );
        id
    }

    fn posts(&self) -> Vec<Value> {
        self.0.lock().unwrap().posts.clone()
    }

    fn fail_next(&self, status: u16, message: &'static str, still_create: bool) {
        self.0.lock().unwrap().next = Some((status, message, still_create));
    }
}

async fn send(
    State(f): State<Fake>,
    h: HeaderMap,
    Path(account): Path<String>,
    Json(body): Json<Value>,
) -> Reply {
    if !authed(&h) {
        return unauthorized();
    }
    f.pay(&account, body, false)
}

async fn request(
    State(f): State<Fake>,
    h: HeaderMap,
    Path(account): Path<String>,
    Json(body): Json<Value>,
) -> Reply {
    if !authed(&h) {
        return unauthorized();
    }
    f.pay(&account, body, true)
}

async fn transaction(State(f): State<Fake>, Path((_, id)): Path<(String, String)>) -> Reply {
    match f.0.lock().unwrap().transactions.get(&id) {
        Some(t) => (StatusCode::OK, Json(t.clone())),
        None => (StatusCode::NOT_FOUND, Json(json!({}))),
    }
}

async fn by_request(
    State(f): State<Fake>,
    Path(_): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Reply {
    let b = f.0.lock().unwrap();
    let found: Vec<Value> = b
        .transactions
        .values()
        .filter(|t| t["requestId"].as_str() == q.get("requestId").map(String::as_str))
        .cloned()
        .collect();
    (StatusCode::OK, Json(json!({ "transactions": found })))
}

async fn get_request(State(f): State<Fake>, Path(id): Path<String>) -> Reply {
    match f.0.lock().unwrap().requests.get(&id) {
        Some(r) => (StatusCode::OK, Json(r.clone())),
        None => (StatusCode::NOT_FOUND, Json(json!({}))),
    }
}

async fn fake_mercury() -> (Fake, String) {
    let fake = Fake::default();
    let app = axum::Router::new()
        .route("/accounts", get(accounts))
        .route("/recipients", get(recipients))
        .route("/recipient/{id}", get(recipient))
        .route(
            "/account/{account}/transactions",
            post(send).get(by_request),
        )
        .route("/account/{account}/request-send-money", post(request))
        .route("/account/{account}/transaction/{id}", get(transaction))
        .route("/request-send-money/{id}", get(get_request))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fake, format!("http://{addr}"))
}

async fn db() -> Option<(TestDb, Fake)> {
    let mut db = TestDb::new().await?;
    let (fake, base) = fake_mercury().await;
    db.state = db.state.clone().with_bank(Bank {
        sealer: Sealer::from_hex(&"5a".repeat(32)).unwrap(),
        mercury_production: base.clone(),
        mercury_sandbox: base.clone(),
        rho_production: base.clone(),
        rho_sandbox: base,
    });
    Some((db, fake))
}

async fn connect(db: &TestDb, mode: &str) {
    commit(
        &db.state,
        &person("robert"),
        "bank.connect",
        json!({ "provider": "mercury", "token": TOKEN, "mode": mode, "account_id": CHECKING }),
    )
    .await
    .unwrap();
}

/// King Environmental, paid by ACH to the recipient in Mercury, with bills
/// that wait for a manual release.
async fn king_vendor(db: &TestDb) -> String {
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "King Environmental").await;
    set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let p = commit(
        &db.state,
        &agent("claude-ap"),
        "vendor.propose_payee",
        json!({ "vendor_id": v, "recipient_id": KING, "method": "ach", "reason": "from Mercury" }),
    )
    .await
    .unwrap();
    let id = p["result"]["proposal"]["id"].as_str().unwrap().to_owned();
    commit(
        &db.state,
        &robert,
        "vendor.accept_payee",
        json!({ "proposal_id": id }),
    )
    .await
    .unwrap();
    v
}

async fn bill(db: &TestDb, v: &str) -> String {
    common::bill(&db.state, &agent("claude-ap"), v, "189727", 45_900).await
}

async fn get_bill(db: &TestDb, id: &str) -> Value {
    commit(
        &db.state,
        &person("robert"),
        "bill.get",
        json!({ "bill_id": id }),
    )
    .await
    .unwrap()["result"]
        .clone()
}

async fn release(db: &TestDb, id: &str) -> Value {
    confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": id }),
    )
    .await
    .unwrap()["result"]
        .clone()
}

/// Lets the payment checker see every payment again, and runs it.
async fn check(db: &TestDb) -> usize {
    db.advance(1);
    payment_worker::tick(&db.state).await.unwrap()
}

#[tokio::test]
async fn connecting_needs_a_person_a_good_token_and_an_account() {
    let Some((db, _)) = db().await else { return };
    let robert = person("robert");

    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "bank.connect",
        json!({ "provider": "mercury", "token": TOKEN }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    let e = commit(
        &db.state,
        &robert,
        "bank.connect",
        json!({ "provider": "mercury", "token": "secret-token:wrong" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("refused this token"), "{e}");

    // Two active accounts: it asks which, naming them.
    let e = commit(
        &db.state,
        &robert,
        "bank.connect",
        json!({ "provider": "mercury", "token": TOKEN }),
    )
    .await
    .unwrap_err();
    assert!(
        e.to_string().contains("Ops Checking") && !e.to_string().contains("Old"),
        "{e}"
    );

    connect(&db, "direct").await;
    let got = commit(&db.state, &agent("claude-ap"), "bank.get", json!({}))
        .await
        .unwrap();
    assert_eq!(
        got["result"]["connection"]["account_name"],
        "Ops Checking ••1234"
    );
    assert!(!got.to_string().contains("mercury_sandbox_good"));

    // Recipients show the last four, never the account number.
    let r = commit(
        &db.state,
        &agent("claude-ap"),
        "bank.recipients",
        json!({ "search": "king" }),
    )
    .await
    .unwrap();
    let shown = r["result"].to_string();
    assert!(
        shown.contains("…6789") && !shown.contains("123456789"),
        "{shown}"
    );
    db.drop().await;
}

#[tokio::test]
async fn payees_are_proposed_by_anyone_and_accepted_by_a_person() {
    let Some((db, _)) = db().await else { return };
    connect(&db, "direct").await;
    let robert = person("robert");
    let ap = agent("claude-ap");
    let v = vendor(&db.state, &robert, "King Environmental").await;

    let e = commit(
        &db.state,
        &ap,
        "vendor.propose_payee",
        json!({ "vendor_id": v, "recipient_id": KING, "method": "domestic_wire" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("can be paid by ach, check"), "{e}");

    let p = commit(
        &db.state,
        &ap,
        "vendor.propose_payee",
        json!({ "vendor_id": v, "recipient_id": KING, "method": "ach" }),
    )
    .await
    .unwrap();
    let id = p["result"]["proposal"]["id"].as_str().unwrap().to_owned();
    assert_eq!(p["result"]["proposal"]["destination"], "…6789");
    let e = commit(
        &db.state,
        &ap,
        "vendor.accept_payee",
        json!({ "proposal_id": id }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    commit(
        &db.state,
        &robert,
        "vendor.accept_payee",
        json!({ "proposal_id": id }),
    )
    .await
    .unwrap();
    let got = commit(&db.state, &ap, "vendor.get", json!({ "vendor_id": v }))
        .await
        .unwrap();
    assert_eq!(got["result"]["payees"][0]["status"], "active");
    db.drop().await;
}

#[tokio::test]
async fn a_direct_release_pays_and_the_bank_says_when_it_went() {
    let Some((db, fake)) = db().await else { return };
    connect(&db, "direct").await;
    let v = king_vendor(&db).await;
    let b = bill(&db, &v).await;

    // The preview shows what will be sent and sends nothing.
    let preview = payvy_server::commands::run(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": b }),
        payvy_server::commands::Mode::Preview,
    )
    .await
    .unwrap();
    let plan = &preview["result"]["payment_plan"];
    assert_eq!(
        (
            plan["to"].as_str(),
            plan["destination"].as_str(),
            plan["amount_cents"].as_i64()
        ),
        (Some("King Environmental"), Some("…6789"), Some(45_900))
    );
    assert!(fake.posts().is_empty());

    let out = release(&db, &b).await;
    let payment = out["payments"][0].clone();
    assert_eq!(payment["status"], "pending");
    assert_eq!(payment["transaction_id"], "txn-1");
    let sent = fake.posts().remove(0);
    assert_eq!(sent["amount"], 459.0);
    assert_eq!(sent["paymentMethod"], "ach");
    assert_eq!(sent["externalMemo"], "Invoice 189727");
    assert!(sent["idempotencyKey"].as_str().unwrap().ends_with("-1"));

    // While the bank works on it, it can't also be marked paid by hand.
    let e = confirm(
        &db.state,
        &person("robert"),
        "bill.mark_paid",
        json!({ "bill_id": b, "paid_on": "2027-01-16", "method": "check" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    assert_eq!(check(&db).await, 0);
    fake.set_transaction("txn-1", "sent");
    assert_eq!(check(&db).await, 1);
    let got = get_bill(&db, &b).await;
    assert_eq!(got["bill"]["state"], "paid");
    assert_eq!(got["bill"]["paid_reference"], "txn-1");

    // An ACH return reopens it, waiting for a person to release it again.
    fake.set_transaction("txn-1", "reversed");
    assert_eq!(check(&db).await, 1);
    assert_eq!(
        status(&db.state, &person("robert"), &b).await,
        "awaiting_release"
    );
    let history = get_bill(&db, &b).await["history"].to_string();
    assert!(history.contains("bill.payment_returned"), "{history}");
    db.drop().await;
}

#[tokio::test]
async fn in_request_mode_the_payment_waits_for_approval_in_mercury() {
    let Some((db, fake)) = db().await else { return };
    connect(&db, "request").await;
    let v = king_vendor(&db).await;
    let b = bill(&db, &v).await;

    let out = release(&db, &b).await;
    assert_eq!(out["payments"][0]["status"], "awaiting_bank_approval");
    assert_eq!(out["payments"][0]["request_id"], "req-1");
    assert_eq!(check(&db).await, 0);

    let txn = fake.approve("req-1");
    assert_eq!(check(&db).await, 1);
    let got = get_bill(&db, &b).await;
    assert_eq!(got["payments"][0]["status"], "pending");
    assert_eq!(got["payments"][0]["transaction_id"], txn.as_str());

    fake.set_transaction(&txn, "sent");
    assert_eq!(check(&db).await, 1);
    assert_eq!(get_bill(&db, &b).await["bill"]["state"], "paid");
    db.drop().await;
}

#[tokio::test]
async fn a_refused_payment_sends_the_bill_back_and_the_next_try_is_new() {
    let Some((db, fake)) = db().await else { return };
    connect(&db, "direct").await;
    let v = king_vendor(&db).await;
    let b = bill(&db, &v).await;

    fake.fail_next(400, "duplicate payment within 24 hours", false);
    let out = release(&db, &b).await;
    assert_eq!(out["payments"][0]["status"], "failed");
    assert!(
        out["payments"][0]["failure"]
            .as_str()
            .unwrap()
            .contains("duplicate"),
        "{out:#}"
    );
    assert_eq!(
        status(&db.state, &person("robert"), &b).await,
        "awaiting_release"
    );

    let out = release(&db, &b).await;
    assert_eq!(out["payments"][0]["status"], "pending");
    let keys: Vec<String> = fake
        .posts()
        .iter()
        .map(|p| p["idempotencyKey"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        keys[0].ends_with("-1") && keys[1].ends_with("-2"),
        "{keys:?}"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_lost_answer_is_resolved_with_the_same_key() {
    let Some((db, fake)) = db().await else { return };
    connect(&db, "direct").await;
    let v = king_vendor(&db).await;
    let b = bill(&db, &v).await;

    // Mercury made the payment, but the answer never arrived.
    fake.fail_next(502, "bad gateway", true);
    let out = release(&db, &b).await;
    assert_eq!(out["payments"][0]["status"], "submitting");

    // Asking again with the same key finds it instead of paying twice.
    assert_eq!(check(&db).await, 1);
    let got = get_bill(&db, &b).await;
    assert_eq!(got["payments"][0]["status"], "pending");
    assert_eq!(got["payments"][0]["transaction_id"], "txn-1");
    let posts = fake.posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0]["idempotencyKey"], posts[1]["idempotencyKey"]);
    assert_eq!(fake.0.lock().unwrap().transactions.len(), 1);
    db.drop().await;
}

#[tokio::test]
async fn without_a_payee_a_release_says_how_to_pay() {
    let Some((db, fake)) = db().await else { return };
    connect(&db, "direct").await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Desert Plumbing").await;
    set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let b = bill(&db, &v).await;
    let e = confirm(&db.state, &robert, "bill.release", json!({ "bill_id": b }))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("vendor.propose_payee"), "{e}");

    let out = confirm(
        &db.state,
        &robert,
        "bill.release",
        json!({ "bill_id": b, "pay_elsewhere": true }),
    )
    .await
    .unwrap();
    assert_eq!(out["result"]["progress"]["status"], "released");
    assert!(
        out["result"]["next"]
            .as_str()
            .unwrap()
            .contains("bill.mark_paid")
    );
    assert!(fake.posts().is_empty());
    db.drop().await;
}

#[tokio::test]
async fn without_a_secrets_key_banks_are_off() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let e = commit(
        &db.state,
        &person("robert"),
        "bank.connect",
        json!({ "provider": "mercury", "token": TOKEN }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::NotConfigured(_)), "{e}");
    db.drop().await;
}
