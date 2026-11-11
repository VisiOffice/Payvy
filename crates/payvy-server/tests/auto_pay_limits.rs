//! The ceiling on automatic payments, against a real database and a small
//! fake Mercury (direct mode). No real bank is called.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use payvy_server::commands::{self, CommandError, Mode};
use payvy_server::secrets::Sealer;
use payvy_server::{Bank, payment_worker};
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore};

mod common;
use common::{TestDb, agent, commit, confirm, person, set_workflow, status, vendor};

const TOKEN: &str = "secret-token:mercury_sandbox_good";
const CHECKING: &str = "acc-checking";
const KING: &str = "rcp-king";

#[derive(Clone, Default)]
struct Fake {
    posts: Arc<Mutex<Vec<Value>>>,
    /// While set, each payment waits for a permit before Mercury answers.
    gate: Arc<Mutex<Option<Arc<Semaphore>>>>,
    arrived: Arc<Notify>,
}

type Reply = (StatusCode, Json<Value>);

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
            { "id": CHECKING, "name": "Ops Checking ••1234", "status": "active", "type": "mercury",
              "kind": "checking", "availableBalance": 52000.5, "canSendRealTimePayments": true }
        ] })),
    )
}

async fn recipient(Path(id): Path<String>) -> Reply {
    if id != KING {
        return (StatusCode::NOT_FOUND, Json(json!({})));
    }
    (
        StatusCode::OK,
        Json(
            json!({ "id": KING, "name": "King Environmental", "status": "active",
            "emails": ["ar@kingenv.com"],
            "electronicRoutingInfo": { "accountNumber": "000123456789", "routingNumber": "121000248",
                                       "electronicAccountType": "businessChecking" } }),
        ),
    )
}

async fn send(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Reply {
    if !authed(&h) {
        return (StatusCode::UNAUTHORIZED, Json(json!({})));
    }
    let gate = f.gate.lock().unwrap().clone();
    if let Some(g) = gate {
        f.arrived.notify_one();
        g.acquire().await.unwrap().forget();
    }
    let mut posts = f.posts.lock().unwrap();
    posts.push(body.clone());
    let id = format!("txn-{}", posts.len());
    (
        StatusCode::OK,
        Json(
            json!({ "id": id, "status": "pending", "amount": body["amount"],
            "counterpartyId": body["recipientId"], "counterpartyName": "King Environmental",
            "createdAt": "2027-01-15T00:00:00Z", "estimatedDeliveryDate": "2027-01-17T00:00:00Z" }),
        ),
    )
}

async fn db() -> Option<(TestDb, Fake)> {
    let mut db = TestDb::new().await?;
    let fake = Fake::default();
    let app = axum::Router::new()
        .route("/accounts", get(accounts))
        .route("/recipient/{id}", get(recipient))
        .route("/account/{account}/transactions", post(send))
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
    });
    commit(
        &db.state,
        &person("robert"),
        "bank.connect",
        json!({ "provider": "mercury", "token": TOKEN, "mode": "direct", "account_id": CHECKING }),
    )
    .await
    .unwrap();
    Some((db, fake))
}

/// A vendor whose bills pay automatically, with an accepted payee.
async fn auto_vendor(db: &TestDb, name: &str) -> String {
    let robert = person("robert");
    let v = vendor(&db.state, &robert, name).await;
    set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": "auto" }]),
    )
    .await;
    let p = commit(
        &db.state,
        &agent("claude-ap"),
        "vendor.propose_payee",
        json!({ "vendor_id": v, "recipient_id": KING, "method": "ach" }),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &robert,
        "vendor.accept_payee",
        json!({ "proposal_id": p["result"]["proposal"]["id"] }),
    )
    .await
    .unwrap();
    v
}

async fn bill(db: &TestDb, v: &str, number: &str, cents: i64) -> String {
    common::bill(&db.state, &agent("claude-ap"), v, number, cents).await
}

async fn progress(db: &TestDb, id: &str) -> Value {
    commit(
        &db.state,
        &person("robert"),
        "bill.get",
        json!({ "bill_id": id }),
    )
    .await
    .unwrap()["result"]["progress"]
        .clone()
}

#[tokio::test]
async fn a_bill_over_the_default_limit_waits_for_a_person_and_says_why() {
    let Some((db, fake)) = db().await else { return };
    let robert = person("robert");
    let limits = commit(&db.state, &agent("claude-ap"), "auto_pay.limits", json!({}))
        .await
        .unwrap();
    assert_eq!(limits["result"]["company"]["limit_cents"], 500_000);
    assert_eq!(limits["result"]["company"]["source"], "default");

    let v = auto_vendor(&db, "King Environmental").await;
    let at = bill(&db, &v, "at-limit", 500_000).await;
    let over = bill(&db, &v, "over-limit", 500_001).await;
    assert_eq!(status(&db.state, &robert, &at).await, "ready_to_pay");
    let p = progress(&db, &over).await;
    assert_eq!(p["status"], "awaiting_release");
    assert_eq!(p["reason"], "Over the company's $5,000.00 auto-pay limit");

    // It is in the inbox as a release, with the reason.
    let inbox = commit(&db.state, &robert, "inbox.get", json!({}))
        .await
        .unwrap();
    let item = inbox["result"]["bills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["bill"]["id"] == over)
        .cloned()
        .expect("the over-limit bill waits on a person");
    assert_eq!(item["action"], "release");
    assert_eq!(
        item["progress"]["reason"],
        "Over the company's $5,000.00 auto-pay limit"
    );

    // The worker pays only the bill at the limit.
    assert_eq!(payment_worker::auto_pay(&db.state).await.unwrap(), 1);
    assert_eq!(fake.posts.lock().unwrap().len(), 1);
    assert_eq!(fake.posts.lock().unwrap()[0]["amount"], 5000.0);
    // Run directly, auto-pay refuses and says why; nothing is sent.
    let e = commit(
        &db.state,
        &agent("bot"),
        "bill.auto_pay",
        json!({ "bill_id": over }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    assert!(
        e.to_string().contains("company's $5,000.00 auto-pay limit"),
        "{e}"
    );
    assert_eq!(fake.posts.lock().unwrap().len(), 1);

    // A person releases it, and that payment goes.
    let out = confirm(
        &db.state,
        &robert,
        "bill.release",
        json!({ "bill_id": over }),
    )
    .await
    .unwrap();
    assert_eq!(out["result"]["payments"][0]["status"], "pending");
    assert_eq!(out["result"]["bill"]["released_by"], "robert");
    assert_eq!(fake.posts.lock().unwrap().len(), 2);
    db.drop().await;
}

#[tokio::test]
async fn only_a_person_changes_the_limit_and_raising_it_is_confirmed() {
    let Some((db, fake)) = db().await else { return };
    let robert = person("robert");
    let v = auto_vendor(&db, "King Environmental").await;
    let big = bill(&db, &v, "big", 750_000).await;
    assert_eq!(status(&db.state, &robert, &big).await, "awaiting_release");

    // Agents can't change it either way, and don't see the commands.
    for name in ["auto_pay.lower_limit", "auto_pay.raise_limit"] {
        let e = commit(
            &db.state,
            &agent("claude-ap"),
            name,
            json!({ "limit_cents": 1_000_000 }),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    }
    // Raising needs a preview and its token.
    let e = commit(
        &db.state,
        &robert,
        "auto_pay.raise_limit",
        json!({ "limit_cents": 1_000_000 }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::NeedsConfirmation(_)), "{e}");
    let preview = commands::run(
        &db.state,
        &robert,
        "auto_pay.raise_limit",
        json!({ "limit_cents": 1_000_000 }),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(
        preview["result"]["change"]["now_pays_automatically"][0]["bill_id"],
        big
    );
    // The preview changed nothing.
    assert_eq!(status(&db.state, &robert, &big).await, "awaiting_release");
    // Lowering through the raise command, or raising through lower, is refused.
    let e = commit(
        &db.state,
        &robert,
        "auto_pay.lower_limit",
        json!({ "limit_cents": 1_000_000 }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    // No "no limit" for the company, and nothing out of range.
    for bad in [json!(null), json!(-1), json!(100_000_000_001_i64)] {
        let e = confirm(
            &db.state,
            &robert,
            "auto_pay.raise_limit",
            json!({ "limit_cents": bad }),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, CommandError::Invalid(_)), "{bad}: {e}");
    }

    let raised = confirm(
        &db.state,
        &robert,
        "auto_pay.raise_limit",
        json!({ "limit_cents": 1_000_000, "reason": "monthly rent" }),
    )
    .await
    .unwrap();
    assert_eq!(raised["result"]["company"]["limit_cents"], 1_000_000);
    assert_eq!(raised["result"]["company"]["source"], "company");
    assert_eq!(raised["events"], json!(["auto_pay.limit_raised"]));
    assert_eq!(status(&db.state, &robert, &big).await, "ready_to_pay");

    // Lowering is immediate and puts the bill back with a person.
    let lowered = commit(
        &db.state,
        &robert,
        "auto_pay.lower_limit",
        json!({ "limit_cents": 250_000 }),
    )
    .await
    .unwrap();
    assert_eq!(
        lowered["result"]["change"]["now_waits_for_a_person"][0]["bill_id"],
        big
    );
    let p = progress(&db, &big).await;
    assert_eq!(p["reason"], "Over the company's $2,500.00 auto-pay limit");
    assert_eq!(payment_worker::auto_pay(&db.state).await.unwrap(), 0);
    assert!(fake.posts.lock().unwrap().is_empty());

    // Both changes are on the record, by the person who made them.
    let log = commit(&db.state, &robert, "event.list", json!({}))
        .await
        .unwrap();
    let changes: Vec<&Value> = log["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"].as_str().unwrap().starts_with("auto_pay."))
        .collect();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["kind"], "auto_pay.limit_lowered");
    assert_eq!(changes[0]["actor"], "robert");
    assert_eq!(changes[0]["payload"]["from_cents"], 1_000_000);
    assert_eq!(changes[0]["payload"]["to_cents"], 250_000);
    assert_eq!(changes[1]["kind"], "auto_pay.limit_raised");
    assert_eq!(changes[1]["payload"]["from_cents"], 500_000);
    assert_eq!(changes[1]["payload"]["reason"], "monthly rent");
    db.drop().await;
}

#[tokio::test]
async fn a_vendor_limit_overrides_the_company_limit_either_way() {
    let Some((db, fake)) = db().await else { return };
    let robert = person("robert");
    let king = auto_vendor(&db, "King Environmental").await;
    let other = auto_vendor(&db, "Other Hauler").await;

    // A vendor without its own limit has nothing to remove.
    let e = commit(
        &db.state,
        &robert,
        "auto_pay.lower_limit",
        json!({ "vendor_id": king, "limit_cents": null }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    // Lower than the company's: routine, and holds this vendor's bills only.
    commit(
        &db.state,
        &robert,
        "auto_pay.lower_limit",
        json!({ "vendor_id": king, "limit_cents": 10_000 }),
    )
    .await
    .unwrap();
    let held = bill(&db, &king, "k-1", 45_900).await;
    let free = bill(&db, &other, "o-1", 45_900).await;
    let p = progress(&db, &held).await;
    assert_eq!(p["status"], "awaiting_release");
    assert_eq!(p["reason"], "Over this vendor's $100.00 auto-pay limit");
    assert_eq!(status(&db.state, &robert, &free).await, "ready_to_pay");

    // Raising an override, even above the company's, needs a confirmation.
    let raise = json!({ "vendor_id": king, "limit_cents": 2_000_000 });
    let e = commit(&db.state, &robert, "auto_pay.raise_limit", raise.clone())
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::NeedsConfirmation(_)), "{e}");
    let e = commit(&db.state, &robert, "auto_pay.lower_limit", raise.clone())
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    let preview = commands::run(
        &db.state,
        &robert,
        "auto_pay.raise_limit",
        raise.clone(),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(
        preview["result"]["change"]["now_pays_automatically"][0]["bill_id"],
        held
    );
    let e = confirm(
        &db.state,
        &robert,
        "auto_pay.raise_limit",
        json!({ "vendor_id": king, "limit_cents": 100_000_000_001_i64 }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    confirm(&db.state, &robert, "auto_pay.raise_limit", raise)
        .await
        .unwrap();

    // Above the company's $5,000.00: this vendor's big bill pays itself,
    // another vendor's waits, naming the company's limit.
    let big_king = bill(&db, &king, "k-big", 1_500_000).await;
    let big_other = bill(&db, &other, "o-big", 1_500_000).await;
    assert_eq!(status(&db.state, &robert, &big_king).await, "ready_to_pay");
    assert_eq!(
        progress(&db, &big_other).await["reason"],
        "Over the company's $5,000.00 auto-pay limit"
    );
    let got = commit(
        &db.state,
        &robert,
        "vendor.get",
        json!({ "vendor_id": king }),
    )
    .await
    .unwrap();
    assert_eq!(got["result"]["auto_pay_limit"]["own_cents"], 2_000_000);
    assert_eq!(
        got["result"]["auto_pay_limit"]["in_force"]["source"],
        "vendor"
    );

    // Lowering the company's limit leaves the override alone.
    let lowered = commit(
        &db.state,
        &robert,
        "auto_pay.lower_limit",
        json!({ "limit_cents": 100_000 }),
    )
    .await
    .unwrap();
    assert_eq!(lowered["result"]["vendors"][0]["limit_cents"], 2_000_000);
    assert_eq!(status(&db.state, &robert, &big_king).await, "ready_to_pay");
    assert_eq!(status(&db.state, &robert, &held).await, "ready_to_pay");
    // Raising the company's limit counts only vendors without an override.
    let preview = commands::run(
        &db.state,
        &robert,
        "auto_pay.raise_limit",
        json!({ "limit_cents": 2_000_000 }),
        Mode::Preview,
    )
    .await
    .unwrap();
    let started: Vec<&Value> = preview["result"]["change"]["now_pays_automatically"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| &b["bill_id"])
        .collect();
    assert_eq!(started, [&json!(big_other)]);

    // Removing an override above the company's lowers what applies: routine.
    commit(
        &db.state,
        &robert,
        "auto_pay.lower_limit",
        json!({ "vendor_id": king, "limit_cents": null }),
    )
    .await
    .unwrap();
    assert_eq!(
        progress(&db, &big_king).await["reason"],
        "Over the company's $1,000.00 auto-pay limit"
    );
    // Removing one below the company's raises it: confirmed.
    commit(
        &db.state,
        &robert,
        "auto_pay.lower_limit",
        json!({ "vendor_id": other, "limit_cents": 10_000 }),
    )
    .await
    .unwrap();
    let remove = json!({ "vendor_id": other, "limit_cents": null });
    let e = commit(&db.state, &robert, "auto_pay.lower_limit", remove.clone())
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    let removed = confirm(&db.state, &robert, "auto_pay.raise_limit", remove)
        .await
        .unwrap();
    assert_eq!(removed["result"]["vendors"], json!([]));

    // Every change shows in the vendor's history.
    let history = commit(
        &db.state,
        &robert,
        "event.list",
        json!({ "entity_id": king }),
    )
    .await
    .unwrap();
    assert!(history.to_string().contains("auto_pay.limit_raised"));
    assert!(history.to_string().contains("auto_pay.limit_lowered"));

    // $1,000.00 company limit: the two $459 bills pay, the big ones wait.
    assert_eq!(payment_worker::auto_pay(&db.state).await.unwrap(), 2);
    assert_eq!(fake.posts.lock().unwrap().len(), 2);
    db.drop().await;
}

#[tokio::test]
async fn the_limit_is_checked_as_each_payment_is_sent_not_when_listed() {
    let Some((db, fake)) = db().await else { return };
    let v = auto_vendor(&db, "King Environmental").await;
    let a = bill(&db, &v, "a", 40_000).await;
    let b = bill(&db, &v, "b", 45_000).await;

    // The worker lists both as ready, then sends the first; Mercury is slow.
    let gate = Arc::new(Semaphore::new(0));
    *fake.gate.lock().unwrap() = Some(gate.clone());
    let state = db.state.clone();
    let worker = tokio::spawn(async move { payment_worker::auto_pay(&state).await.unwrap() });
    fake.arrived.notified().await;

    // A person lowers the limit below both bills meanwhile. The change waits
    // for the payment already being sent, decided under the old limit...
    let state = db.state.clone();
    let lower = tokio::spawn(async move {
        commit(
            &state,
            &person("robert"),
            "auto_pay.lower_limit",
            json!({ "limit_cents": 10_000 }),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !lower.is_finished(),
        "a limit change waits for a payment being sent"
    );
    gate.add_permits(10);

    // ...and the second bill, listed as ready before the change, isn't paid.
    assert_eq!(worker.await.unwrap(), 1);
    let lowered = lower.await.unwrap().unwrap();
    assert_eq!(fake.posts.lock().unwrap().len(), 1);
    let robert = person("robert");
    let sa = status(&db.state, &robert, &a).await;
    let first_was_a = sa == "released";
    let (paid, waiting) = if first_was_a { (a, b) } else { (b, a) };
    assert_eq!(status(&db.state, &robert, &paid).await, "released");
    assert_eq!(
        status(&db.state, &robert, &waiting).await,
        "awaiting_release"
    );
    assert_eq!(
        lowered["result"]["change"]["now_waits_for_a_person"],
        json!([{ "bill_id": waiting, "vendor_name": "King Environmental",
                  "invoice_number": if first_was_a { "b" } else { "a" },
                  "amount_cents": if first_was_a { 45_000 } else { 40_000 } }])
    );
    db.drop().await;
}
