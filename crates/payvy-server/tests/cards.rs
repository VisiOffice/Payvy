//! Cards on file: a vendor charges a card the company keeps with it, and
//! Payvy pairs each charge with a bill only when the answer is plain.
//! Against a real database and a fake Mercury with cards and card
//! transactions; no real bank is called.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use payvy_server::commands::CommandError;
use payvy_server::secrets::Sealer;
use payvy_server::{Bank, payment_worker};
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, commit, confirm, person, set_workflow, vendor};

const TOKEN: &str = "secret-token:mercury_sandbox_good";
const CARD: &str = "card-localbin";

#[derive(Default)]
struct Bank0 {
    transactions: Vec<Value>,
    refuse_cards: bool,
    /// The query strings `/transactions` was called with.
    asked: Vec<String>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Bank0>>);

type Reply = (StatusCode, Json<Value>);

impl Fake {
    fn charge(&self, id: &str, card: &str, dollars: f64, status: &str, on: &str) {
        let mut b = self.0.lock().unwrap();
        b.transactions.retain(|t| t["id"] != id);
        b.transactions.push(json!({
            "id": id, "cardId": card, "amount": dollars, "status": status,
            "kind": "creditCardTransaction", "counterpartyName": "LOCAL BIN RENTALS",
            "createdAt": format!("{on}T17:00:00Z"),
            "postedAt": if status == "sent" { json!(format!("{on}T23:00:00Z")) } else { json!(null) },
            "dashboardLink": format!("https://mercury.example/{id}")
        }));
    }
}

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

async fn accounts(h: HeaderMap) -> Reply {
    if !authed(&h) {
        return (StatusCode::UNAUTHORIZED, Json(json!({})));
    }
    (
        StatusCode::OK,
        Json(json!({ "accounts": [
            { "id": "acc-checking", "name": "Checking ••2393", "status": "active", "kind": "checking",
              "availableBalance": 1000.0, "canSendRealTimePayments": false }
        ] })),
    )
}

async fn cards(State(f): State<Fake>, h: HeaderMap) -> Reply {
    if !authed(&h) || f.0.lock().unwrap().refuse_cards {
        return (
            StatusCode::FORBIDDEN,
            Json(
                json!({ "errors": { "message": "You must have a token with access to this endpoint" } }),
            ),
        );
    }
    (
        StatusCode::OK,
        Json(json!({ "cards": [
            { "id": CARD, "lastFour": "4242", "nickname": "Local Bin", "nameOnCard": "Robert",
              "kind": "credit", "status": "active", "type": "virtual" },
            { "id": "card-other", "lastFour": "1111", "nickname": "Fuel", "kind": "debit",
              "status": "active", "type": "physical" },
            { "id": "card-dead", "lastFour": "9999", "kind": "debit", "status": "cancelled", "type": "virtual" }
        ], "page": {} })),
    )
}

async fn transactions(State(f): State<Fake>, RawQuery(q): RawQuery) -> Reply {
    let q = q.unwrap_or_default();
    let mut b = f.0.lock().unwrap();
    b.asked.push(q.clone());
    let wanted: Vec<String> = q
        .split('&')
        .filter_map(|p| p.strip_prefix("cardId="))
        .map(str::to_owned)
        .collect();
    let found: Vec<Value> = b
        .transactions
        .iter()
        .filter(|t| wanted.iter().any(|w| t["cardId"] == w.as_str()))
        .cloned()
        .collect();
    (
        StatusCode::OK,
        Json(json!({ "transactions": found, "page": {} })),
    )
}

async fn db() -> Option<(TestDb, Fake)> {
    let mut db = TestDb::new().await?;
    let fake = Fake::default();
    let app = axum::Router::new()
        .route("/accounts", get(accounts))
        .route("/cards", get(cards))
        .route("/transactions", get(transactions))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    db.state = db.state.clone().with_bank(Bank {
        ramp: None,
        sealer: Sealer::from_hex(&"5a".repeat(32)).unwrap(),
        mercury_production: base.clone(),
        mercury_sandbox: base.clone(),
        rho_production: base.clone(),
        rho_sandbox: base,
        webhook_base: None,
        outbound_ips: vec![],
    });
    commit(
        &db.state,
        &person("robert"),
        "bank.connect",
        json!({ "provider": "mercury", "token": TOKEN, "mode": "request" }),
    )
    .await
    .unwrap();
    Some((db, fake))
}

/// Local Bin, whose bills wait for a person to release them.
async fn local_bin(db: &TestDb) -> String {
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Local Bin Rentals").await;
    set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    v
}

async fn link(db: &TestDb, v: &str) -> Value {
    commit(
        &db.state,
        &person("robert"),
        "vendor.link_card",
        json!({ "vendor_id": v, "card_id": CARD, "charges_from": "2027-01-01" }),
    )
    .await
    .unwrap()["result"]
        .clone()
}

async fn bill(db: &TestDb, v: &str, number: &str, cents: i64, on: &str) -> String {
    commit(
        &db.state,
        &agent("claude-ap"),
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": number, "invoice_date": on, "amount_cents": cents }),
    )
    .await
    .unwrap()["result"]["bill"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
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

async fn sync(db: &TestDb) -> Value {
    commit(&db.state, &agent("bank"), "card.sync", json!({}))
        .await
        .unwrap()["result"]
        .clone()
}

#[tokio::test]
async fn a_person_links_a_card_and_only_the_last_four_is_kept() {
    let Some((db, fake)) = db().await else { return };
    let v = local_bin(&db).await;

    // Anyone may look; the list never carries a card number.
    let listed = commit(&db.state, &agent("claude-ap"), "bank.cards", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["result"]["cards"][0]["last_four"], "4242");

    // Only a person says which card a vendor keeps.
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "vendor.link_card",
        json!({ "vendor_id": v, "card_id": CARD }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    // A cancelled card can't be charged.
    let e = commit(
        &db.state,
        &person("robert"),
        "vendor.link_card",
        json!({ "vendor_id": v, "card_id": "card-dead" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("cancelled"), "{e}");

    let linked = link(&db, &v).await;
    assert_eq!(linked["card"]["last_four"], "4242");

    // One card, one vendor: another vendor can't share it.
    let other = vendor(&db.state, &person("robert"), "Bills Hauling").await;
    let e = commit(
        &db.state,
        &person("robert"),
        "vendor.link_card",
        json!({ "vendor_id": other, "card_id": CARD }),
    )
    .await
    .unwrap_err();
    assert!(
        e.to_string().contains("already on file with Local Bin"),
        "{e}"
    );

    let listed = commit(&db.state, &agent("claude-ap"), "bank.cards", json!({}))
        .await
        .unwrap();
    assert_eq!(
        listed["result"]["cards"][0]["on_file_with"]["vendor_name"],
        "Local Bin Rentals"
    );
    let shown = commit(
        &db.state,
        &person("robert"),
        "vendor.get",
        json!({ "vendor_id": v }),
    )
    .await
    .unwrap();
    assert_eq!(shown["result"]["card"]["last_four"], "4242");

    // A token that can't read cards says what to do about it.
    fake.0.lock().unwrap().refuse_cards = true;
    let e = commit(&db.state, &agent("claude-ap"), "bank.cards", json!({}))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("permission to read cards"), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn a_charge_pays_the_one_bill_it_plainly_belongs_to_once_released() {
    let Some((db, fake)) = db().await else { return };
    let v = local_bin(&db).await;
    link(&db, &v).await;
    let beverlywood = bill(&db, &v, "8932-beverlywood", 75_000, "2027-01-10").await;
    let sepulveda = bill(&db, &v, "6501-sepulveda", 69_500, "2027-01-08").await;

    // Released: no transfer is sent; the vendor charges the card.
    let released = release(&db, &beverlywood).await;
    assert_eq!(released["card_plan"]["card"], "••4242");
    assert!(released["payments"].as_array().unwrap().is_empty());
    assert_eq!(released["bill"]["state"], "open");

    // Pending at the bank: seen, not yet matched or paid.
    fake.charge("txn-1", CARD, -750.0, "pending", "2027-01-11");
    // A refund, and a charge on a card nobody linked: neither is a bill's.
    fake.charge("txn-refund", CARD, 25.0, "sent", "2027-01-11");
    fake.charge("txn-fuel", "card-other", -750.0, "sent", "2027-01-11");
    let r = sync(&db).await;
    assert_eq!(
        (r["recorded"].as_u64(), r["paid"].as_u64()),
        (Some(1), Some(0))
    );
    assert_eq!(get_bill(&db, &beverlywood).await["bill"]["state"], "open");

    // Posted: it pays the one $750 bill, which was released.
    fake.charge("txn-1", CARD, -750.0, "sent", "2027-01-11");
    let r = sync(&db).await;
    assert_eq!(
        (r["matched"].as_u64(), r["paid"].as_u64()),
        (Some(1), Some(1))
    );
    let b = get_bill(&db, &beverlywood).await;
    assert_eq!(b["bill"]["state"], "paid");
    assert_eq!(b["bill"]["paid_method"], "card");
    assert_eq!(b["bill"]["paid_reference"], "txn-1");
    assert_eq!(b["bill"]["paid_on"], "2027-01-11");
    assert_eq!(b["card_charge"]["rule"], "amount");
    let paid = b["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "bill.paid")
        .unwrap();
    assert_eq!(paid["actor"], "bank");

    // Charged before anyone released it: matched, and paid on release.
    fake.charge("txn-2", CARD, -695.0, "sent", "2027-01-09");
    sync(&db).await;
    assert_eq!(get_bill(&db, &sepulveda).await["bill"]["state"], "open");
    let open = commit(
        &db.state,
        &person("robert"),
        "card.charges",
        json!({ "open": true }),
    )
    .await
    .unwrap();
    let early = &open["result"]["charges"][0];
    assert_eq!(early["transaction_id"], "txn-2");
    assert!(
        early["needs"]
            .as_str()
            .unwrap()
            .contains("before the bill was released")
    );
    let released = release(&db, &sepulveda).await;
    assert_eq!(released["bill"]["state"], "paid");

    // Nothing left to look at; syncing again changes nothing.
    let r = sync(&db).await;
    assert_eq!(
        (r["recorded"].as_u64(), r["paid"].as_u64()),
        (Some(0), Some(0))
    );
    let open = commit(
        &db.state,
        &person("robert"),
        "card.charges",
        json!({ "open": true }),
    )
    .await
    .unwrap();
    assert!(open["result"]["charges"].as_array().unwrap().is_empty());
    db.drop().await;
}

#[tokio::test]
async fn two_bills_for_the_same_amount_wait_for_a_person() {
    let Some((db, fake)) = db().await else { return };
    let v = local_bin(&db).await;
    link(&db, &v).await;
    let first = bill(&db, &v, "A-1", 75_000, "2027-01-10").await;
    let second = bill(&db, &v, "A-2", 75_000, "2027-01-12").await;
    release(&db, &first).await;
    release(&db, &second).await;

    fake.charge("txn-1", CARD, -750.0, "sent", "2027-01-13");
    let r = sync(&db).await;
    assert_eq!(r["matched"].as_u64(), Some(0));
    let open = commit(
        &db.state,
        &person("robert"),
        "card.charges",
        json!({ "open": true }),
    )
    .await
    .unwrap();
    let charge = open["result"]["charges"][0].clone();
    assert!(
        charge["needs"]
            .as_str()
            .unwrap()
            .contains("match it to a bill")
    );
    let charge_id = charge["id"].as_str().unwrap();

    // An agent can't decide; a person can, and the bill is paid.
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "card.match_charge",
        json!({ "charge_id": charge_id, "bill_id": second }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let m = commit(
        &db.state,
        &person("robert"),
        "card.match_charge",
        json!({ "charge_id": charge_id, "bill_id": second }),
    )
    .await
    .unwrap();
    assert_eq!(m["result"]["bill_paid"], true);
    assert_eq!(get_bill(&db, &second).await["bill"]["state"], "paid");
    assert_eq!(get_bill(&db, &first).await["bill"]["state"], "open");

    // Another charge of a different amount pays no bill: dismissed with a reason.
    fake.charge("txn-fee", CARD, -35.0, "sent", "2027-01-14");
    sync(&db).await;
    let open = commit(
        &db.state,
        &person("robert"),
        "card.charges",
        json!({ "open": true }),
    )
    .await
    .unwrap();
    let fee = open["result"]["charges"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let e = commit(
        &db.state,
        &person("robert"),
        "card.match_charge",
        json!({ "charge_id": fee, "bill_id": first }),
    )
    .await
    .unwrap_err();
    assert!(
        e.to_string().contains("one charge pays one whole bill"),
        "{e}"
    );
    commit(
        &db.state,
        &person("robert"),
        "card.dismiss_charge",
        json!({ "charge_id": fee, "reason": "late fee, disputed" }),
    )
    .await
    .unwrap();

    // With one $750 bill left, the next $750 charge is plain.
    fake.charge("txn-3", CARD, -750.0, "sent", "2027-01-15");
    let r = sync(&db).await;
    assert_eq!(r["paid"].as_u64(), Some(1));
    assert_eq!(get_bill(&db, &first).await["bill"]["state"], "paid");
    db.drop().await;
}

#[tokio::test]
async fn a_reversed_charge_reopens_the_bill_it_paid() {
    let Some((db, fake)) = db().await else { return };
    let v = local_bin(&db).await;
    link(&db, &v).await;
    let b = bill(&db, &v, "R-1", 75_000, "2027-01-10").await;
    release(&db, &b).await;
    fake.charge("txn-1", CARD, -750.0, "sent", "2027-01-11");
    sync(&db).await;
    assert_eq!(get_bill(&db, &b).await["bill"]["state"], "paid");

    fake.charge("txn-1", CARD, -750.0, "reversed", "2027-01-11");
    sync(&db).await;
    let got = get_bill(&db, &b).await;
    assert_eq!(got["bill"]["state"], "open");
    assert_eq!(got["progress"]["status"], "awaiting_release");
    assert!(got["card_charge"].is_null());
    db.drop().await;
}

#[tokio::test]
async fn an_automatic_pay_step_clears_the_bill_for_the_charge_and_sends_nothing() {
    let Some((db, fake)) = db().await else { return };
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Local Bin Rentals").await;
    set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "auto" }]),
    )
    .await;
    link(&db, &v).await;
    let b = bill(&db, &v, "AUTO-1", 75_000, "2027-01-10").await;
    payment_worker::auto_pay(&db.state).await.unwrap();
    let got = get_bill(&db, &b).await;
    assert!(got["payments"].as_array().unwrap().is_empty());
    assert_eq!(got["progress"]["status"], "released");

    fake.charge("txn-1", CARD, -750.0, "sent", "2027-01-11");
    assert_eq!(payment_worker::sync_cards(&db.state).await.unwrap(), 1);
    assert_eq!(get_bill(&db, &b).await["bill"]["state"], "paid");

    // Charges from before the card counts aren't asked for.
    assert!(fake.0.lock().unwrap().asked[0].contains("start=2027-01-01"));
    db.drop().await;
}
