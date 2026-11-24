//! "Paid outside Payvy" declarations on purchase orders:
//! blocks payment at once, requires confirmation (or `accept_from_source`
//! policy for the standing-authorization key), and marks the bill paid once
//! effective and released by a person — without ever sending a bank transfer.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use payvy_ledger::FakeLedger;
use payvy_server::Actor;
use payvy_server::Bank;
use payvy_server::commands::CommandError;
use payvy_server::secrets::Sealer;
use payvy_store::{self as store, ActorKind, DEFAULT_ORG, NewLedgerConnection};
use serde_json::{Value, json};
use uuid::Uuid;

mod common;
use common::{TestDb, agent, commit, confirm, person, set_workflow, vendor};

const TOKEN: &str = "secret-token:mercury_sandbox_good";

#[derive(Default)]
struct BankState {
    transfers: Vec<Value>,
}

#[derive(Clone, Default)]
struct FakeBank(Arc<Mutex<BankState>>);

impl FakeBank {
    fn transfers(&self) -> Vec<Value> {
        self.0.lock().unwrap().transfers.clone()
    }
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
            { "id": "acc-checking", "name": "Checking ••2393", "status": "active", "kind": "checking",
              "availableBalance": 100_000.0, "canSendRealTimePayments": false }
        ] })),
    )
}

fn recipient_json() -> Value {
    json!({
        "id": "rec-1",
        "name": "Acme Supply",
        "status": "active",
        "defaultPaymentMethod": "ach",
        "electronicRoutingInfo": {
            "accountNumber": "****4321",
            "routingNumber": "021000021",
            "bankName": "Test Bank",
            "electronicAccountType": "businessChecking"
        }
    })
}

async fn recipients(h: HeaderMap) -> Reply {
    if !authed(&h) {
        return (StatusCode::UNAUTHORIZED, Json(json!({})));
    }
    (
        StatusCode::OK,
        Json(json!({ "recipients": [recipient_json()], "page": {} })),
    )
}

async fn recipient(h: HeaderMap) -> Reply {
    if !authed(&h) {
        return (StatusCode::UNAUTHORIZED, Json(json!({})));
    }
    (StatusCode::OK, Json(recipient_json()))
}

async fn request_send_money(
    State(f): State<FakeBank>,
    h: HeaderMap,
    Json(body): Json<Value>,
) -> Reply {
    if !authed(&h) {
        return (StatusCode::UNAUTHORIZED, Json(json!({})));
    }
    let mut b = f.0.lock().unwrap();
    let id = format!("req-{}", b.transfers.len() + 1);
    b.transfers.push(body.clone());
    (
        StatusCode::OK,
        Json(json!({
            "requestId": id,
            "accountId": "acc-checking",
            "recipientId": body["recipientId"],
            "paymentMethod": body["paymentMethod"],
            "amount": body["amount"],
            "status": "pendingApproval",
            "reviews": [],
            "requestedByUserId": "u1",
            "createdAt": "2026-11-01T12:00:00Z"
        })),
    )
}

async fn setup() -> Option<(TestDb, FakeBank, FakeLedger)> {
    let mut db = TestDb::new().await?;
    let fake_bank = FakeBank::default();
    let fake_ledger = FakeLedger::new();
    let app = axum::Router::new()
        .route("/accounts", get(accounts))
        .route("/recipients", get(recipients))
        .route("/recipient/{id}", get(recipient))
        .route(
            "/account/{account}/request-send-money",
            post(request_send_money),
        )
        .with_state(fake_bank.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let sealer = Sealer::from_hex(&"5a".repeat(32)).unwrap();
    db.state = db
        .state
        .clone()
        .with_bank(Bank {
            ramp: None,
            sealer,
            mercury_production: base.clone(),
            mercury_sandbox: base.clone(),
            rho_production: base.clone(),
            rho_sandbox: base,
            webhook_base: None,
            outbound_ips: vec![],
        })
        .with_ledger(fake_ledger.clone());

    commit(
        &db.state,
        &person("admin"),
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
    for (email, name, actor) in [
        ("robert@example.com", "Robert", "robert"),
        ("alice@example.com", "Alice", "alice"),
    ] {
        commit(
            &db.state,
            &person("admin"),
            "person.add",
            json!({ "email": email, "name": name, "actor": actor, "roles": ["admin"] }),
        )
        .await
        .unwrap();
    }
    commit(
        &db.state,
        &person("robert"),
        "bank.connect",
        json!({ "provider": "mercury", "token": TOKEN, "mode": "request" }),
    )
    .await
    .unwrap();
    Some((db, fake_bank, fake_ledger))
}

async fn setup_vendor_with_payee(db: &TestDb, name: &str, auto_pay: bool) -> String {
    let robert = person("robert");
    let v = vendor(&db.state, &robert, name).await;
    let release = if auto_pay { "auto" } else { "manual" };
    set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([{ "id": "pay", "type": "pay", "release": release }]),
    )
    .await;
    let prop = commit(
        &db.state,
        &robert,
        "vendor.propose_payee",
        json!({ "vendor_id": v, "recipient_id": "rec-1", "method": "ach" }),
    )
    .await
    .unwrap();
    let prop_id = prop["result"]["proposal"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    commit(
        &db.state,
        &person("alice"),
        "vendor.accept_payee",
        json!({ "proposal_id": prop_id }),
    )
    .await
    .unwrap();
    v
}

async fn authorize_source(db: &TestDb, vendor_id: &str, source: &str) -> Actor {
    let key_id = db
        .state
        .store
        .insert_api_key(
            DEFAULT_ORG,
            source,
            ActorKind::Agent,
            &payvy_server::hash_key(&format!("{source}-key-{}", Uuid::new_v4())),
        )
        .await
        .unwrap();
    commit(
        &db.state,
        &person("robert"),
        "po.source_authorize",
        json!({
            "source": source,
            "key_id": key_id,
            "vendor_ids": [vendor_id],
            "max_cents": 500_000,
            "owner": "robert",
        }),
    )
    .await
    .unwrap();
    agent(source).with_key(key_id)
}

async fn upsert_active_po(
    db: &TestDb,
    source_actor: &Actor,
    vendor_id: &str,
    po_number: &str,
    ext_id: &str,
    cents: i64,
) -> String {
    let r = commit(
        &db.state,
        source_actor,
        "po.upsert",
        json!({
            "vendor_id": vendor_id,
            "po_number": po_number,
            "external_source": "orders",
            "external_id": ext_id,
            "authorized_cents": cents,
            "lines": [{ "description": "Service", "amount_cents": cents }],
        }),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["status"], "active");
    r["result"]["id"].as_str().unwrap().to_owned()
}

async fn create_and_match_bill(
    db: &TestDb,
    vendor_id: &str,
    po_id: &str,
    inv: &str,
    cents: i64,
    date: &str,
) -> String {
    let b = commit(
        &db.state,
        &agent("ap-bot"),
        "bill.create",
        json!({
            "vendor_id": vendor_id,
            "invoice_number": inv,
            "invoice_date": date,
            "amount_cents": cents,
        }),
    )
    .await
    .unwrap();
    let bill_id = b["result"]["bill"]["id"].as_str().unwrap().to_owned();
    confirm(
        &db.state,
        &person("robert"),
        "po.match_apply",
        json!({ "bill_id": bill_id, "allocations": [{ "po_id": po_id, "amount_cents": cents }] }),
    )
    .await
    .unwrap();
    bill_id
}

async fn get_bill(db: &TestDb, bill_id: &str) -> Value {
    commit(
        &db.state,
        &person("robert"),
        "bill.get",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap()["result"]
        .clone()
}

/// Test 1: Source key declares; another agent key is refused; a person declares.
#[tokio::test]
async fn source_key_and_person_declare_other_agent_refused() {
    let Some((db, bank, _)) = setup().await else {
        return;
    };
    let v = setup_vendor_with_payee(&db, "Acme Supply", true).await;
    let src = authorize_source(&db, &v, "orders").await;
    let po_id = upsert_active_po(&db, &src, &v, "PO-101", "ext-101", 200_000).await;

    // Source key declares via external_source + external_id.
    let d1 = commit(
        &db.state,
        &src,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-101",
            "amount_cents": 75_000,
            "method": "card",
            "paid_on": "2026-11-01",
            "reference": "AUTH-48291",
            "note": "Charged on phone by staff",
        }),
    )
    .await
    .unwrap();
    assert_eq!(d1["result"]["declaration"]["status"], "declared");
    assert_eq!(d1["result"]["declaration"]["declared_by_source"], true);
    assert_eq!(d1["result"]["declaration"]["reference"], "AUTH-48291");

    // Another agent key is refused (both by external id and by po_id).
    let rogue_key = db
        .state
        .store
        .insert_api_key(
            DEFAULT_ORG,
            "rogue",
            ActorKind::Agent,
            &payvy_server::hash_key("rogue-key"),
        )
        .await
        .unwrap();
    let rogue = agent("rogue").with_key(rogue_key);
    let err_ext = commit(
        &db.state,
        &rogue,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-101",
            "amount_cents": 50_000,
            "method": "check",
            "paid_on": "2026-11-01",
            "note": "Unauthorized agent attempt",
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err_ext, CommandError::Conflict(_)), "{err_ext}");

    let err_id = commit(
        &db.state,
        &rogue,
        "po.declare_paid",
        json!({
            "po_id": po_id,
            "amount_cents": 50_000,
            "method": "check",
            "paid_on": "2026-11-01",
            "note": "Agent passing po_id",
        }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err_id, CommandError::PersonOnly(_)), "{err_id}");

    // A person declares via po_id.
    let d2 = commit(
        &db.state,
        &person("robert"),
        "po.declare_paid",
        json!({
            "po_id": po_id,
            "amount_cents": 50_000,
            "method": "check",
            "paid_on": "2026-11-01",
            "reference": "CHK-1042",
            "note": "Handed check at site",
        }),
    )
    .await
    .unwrap();
    assert_eq!(d2["result"]["declaration"]["declared_by"], "robert");
    assert_eq!(d2["result"]["declaration"]["declared_by_source"], false);
    assert!(bank.transfers().is_empty());
}

/// Test 2: Default policy: declared but unconfirmed -> bill blocked from auto-pay
/// and release sends no transfer; after another person confirms and the bill is
/// released, it is paid with the declaration's method/reference/date and the
/// ledger entry exists. The declarer cannot confirm.
#[tokio::test]
async fn default_policy_blocks_until_other_person_confirms_and_person_releases() {
    let Some((db, bank, _ledger)) = setup().await else {
        return;
    };
    let v = setup_vendor_with_payee(&db, "Acme Supply", true).await;
    let src = authorize_source(&db, &v, "orders").await;
    let po_id = upsert_active_po(&db, &src, &v, "PO-201", "ext-201", 100_000).await;

    // Connect a fake ledger so on_outside_payment enqueues post_bill + post_payment.
    {
        let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
        let sealed = Sealer::from_hex(&"5a".repeat(32))
            .unwrap()
            .seal(
                DEFAULT_ORG,
                &payvy_ledger::seal_purpose("netsuite"),
                "unused",
            )
            .unwrap();
        let conn = NewLedgerConnection {
            id: Uuid::new_v4(),
            origin: None,
            provider: "netsuite",
            environment: "production",
            account_id: "1234567",
            subsidiary_id: Some("1"),
            subsidiary_name: Some("Main"),
            currency: "USD",
            status: "active",
            auth_kind: "client_credentials",
            token_sealed: &sealed,
            post_when: "payment",
            ap_account_id: Some("25"),
            defaults: &json!({ "expense_account_id": "65" }),
            bank_account_map: &json!({}),
            created_at: *db.clock.lock().unwrap(),
            created_by: "robert",
        };
        store::insert_ledger_connection(&mut tx, DEFAULT_ORG, &conn)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    let bill_id = create_and_match_bill(&db, &v, &po_id, "INV-201", 75_000, "2026-11-01").await;

    // Robert declares payment outside Payvy.
    let decl = commit(
        &db.state,
        &person("robert"),
        "po.declare_paid",
        json!({
            "po_id": po_id,
            "amount_cents": 75_000,
            "method": "check",
            "paid_on": "2026-11-02",
            "reference": "CHK-8891",
            "note": "Check handed to driver",
        }),
    )
    .await
    .unwrap();
    let decl_id = decl["result"]["declaration"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Bill evaluates to awaiting_release (never ready_to_pay) and auto_pay refuses.
    let detail = get_bill(&db, &bill_id).await;
    assert_eq!(detail["progress"]["status"], "awaiting_release");
    assert_eq!(detail["outside_payment"]["state"], "awaiting_confirmation");
    let err = commit(
        &db.state,
        &agent("workflow"),
        "bill.auto_pay",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CommandError::Conflict(_)), "{err}");

    // Release is refused while the paired declaration is unconfirmed (including pay_elsewhere: true).
    let rel_unconfirmed = confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(rel_unconfirmed, CommandError::Conflict(_)),
        "{rel_unconfirmed}"
    );
    let rel_elsewhere = confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill_id, "pay_elsewhere": true }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(rel_elsewhere, CommandError::Conflict(_)),
        "{rel_elsewhere}"
    );
    assert!(bank.transfers().is_empty());

    // Declarer (robert) cannot confirm his own declaration.
    let self_confirm = commit(
        &db.state,
        &person("robert"),
        "po.confirm_paid",
        json!({ "declaration_id": decl_id }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(self_confirm, CommandError::Forbidden(_)),
        "{self_confirm}"
    );

    // Agent cannot confirm.
    let agent_confirm = commit(
        &db.state,
        &src,
        "po.confirm_paid",
        json!({ "declaration_id": decl_id }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(agent_confirm, CommandError::PersonOnly(_)),
        "{agent_confirm}"
    );

    // Alice confirms; bill is still waiting for release (not paid yet).
    commit(
        &db.state,
        &person("alice"),
        "po.confirm_paid",
        json!({ "declaration_id": decl_id }),
    )
    .await
    .unwrap();
    let after_confirm = get_bill(&db, &bill_id).await;
    assert_eq!(after_confirm["bill"]["state"], "open");
    assert_eq!(
        after_confirm["outside_payment"]["state"],
        "awaiting_release"
    );

    // Robert releases: sends no bank transfer, marks bill paid immediately.
    let released = confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap();
    assert_eq!(released["result"]["bill"]["state"], "paid");
    assert_eq!(released["result"]["bill"]["paid_method"], "check");
    assert_eq!(released["result"]["bill"]["paid_reference"], "CHK-8891");
    assert_eq!(released["result"]["bill"]["paid_on"], "2026-11-02");
    assert_eq!(released["result"]["outside_payment"]["state"], "paid");
    assert!(bank.transfers().is_empty());

    // Check bill.paid event carries via: "declaration" and declaration_id.
    let paid_ev = released["result"]["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "bill.paid")
        .expect("bill.paid event");
    assert_eq!(paid_ev["payload"]["via"], "declaration");
    assert_eq!(paid_ev["payload"]["declaration_id"], decl_id);

    // Ledger post_payment job was queued by on_outside_payment.
    let now = *db.clock.lock().unwrap();
    let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
    let jobs = store::claim_ledger_jobs(&mut tx, now, 10).await.unwrap();
    tx.rollback().await.unwrap();
    assert!(
        jobs.iter()
            .any(|j| j.kind == "post_payment"
                && j.bill_id == Some(Uuid::parse_str(&bill_id).unwrap())),
        "expected post_payment ledger job, got: {jobs:?}"
    );
}

/// Test 3: `accept_from_source`: source declaration is effective without confirm;
/// a person's declaration still needs confirming.
#[tokio::test]
async fn accept_from_source_policy_applies_only_to_source_key() {
    let Some((db, bank, _)) = setup().await else {
        return;
    };
    let v = setup_vendor_with_payee(&db, "Acme Supply", false).await;
    let src = authorize_source(&db, &v, "orders").await;

    // Enable accept_from_source policy for this vendor.
    let pol = commit(
        &db.state,
        &person("robert"),
        "po.policy_set",
        json!({ "vendor_id": v, "paid_outside": "accept_from_source", "reason": "trust source" }),
    )
    .await
    .unwrap();
    assert_eq!(pol["result"]["paid_outside"], "accept_from_source");

    let po_id = upsert_active_po(&db, &src, &v, "PO-301", "ext-301", 200_000).await;
    let bill1 = create_and_match_bill(&db, &v, &po_id, "INV-301", 60_000, "2026-11-01").await;
    let bill2 = create_and_match_bill(&db, &v, &po_id, "INV-302", 40_000, "2026-11-01").await;

    // Source key declares for bill1 -> effective immediately, no confirm needed.
    commit(
        &db.state,
        &src,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-301",
            "amount_cents": 60_000,
            "method": "card",
            "paid_on": "2026-11-01",
            "note": "Paid at pickup by source system",
        }),
    )
    .await
    .unwrap();
    let rel1 = confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill1 }),
    )
    .await
    .unwrap();
    assert_eq!(rel1["result"]["bill"]["state"], "paid");

    // Person declares for bill2 -> still requires confirmation even under accept_from_source.
    let d2 = commit(
        &db.state,
        &person("robert"),
        "po.declare_paid",
        json!({
            "po_id": po_id,
            "amount_cents": 40_000,
            "method": "cash",
            "paid_on": "2026-11-01",
            "note": "Paid cash",
        }),
    )
    .await
    .unwrap();
    let d2_id = d2["result"]["declaration"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Release is refused while the person's declaration is unconfirmed.
    let rel2_err = confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill2 }),
    )
    .await
    .unwrap_err();
    assert!(matches!(rel2_err, CommandError::Conflict(_)), "{rel2_err}");

    // Once Alice confirms, release settles bill2 immediately (mapping cash -> other on the bill).
    commit(
        &db.state,
        &person("alice"),
        "po.confirm_paid",
        json!({ "declaration_id": d2_id }),
    )
    .await
    .unwrap();
    let rel2 = confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill2 }),
    )
    .await
    .unwrap();
    assert_eq!(rel2["result"]["bill"]["state"], "paid");
    assert_eq!(rel2["result"]["bill"]["paid_method"], "other");
    assert!(bank.transfers().is_empty());
}

/// Test 4: Declaration arrives before the invoice: the later bill, once matched
/// to the PO, is blocked and then settled.
#[tokio::test]
async fn declaration_before_invoice_blocks_and_settles_later_bill() {
    let Some((db, bank, _)) = setup().await else {
        return;
    };
    let v = setup_vendor_with_payee(&db, "Acme Supply", true).await;
    let src = authorize_source(&db, &v, "orders").await;
    let po_id = upsert_active_po(&db, &src, &v, "PO-401", "ext-401", 100_000).await;

    // Declaration arrives before any invoice exists, and is confirmed.
    let decl = commit(
        &db.state,
        &src,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-401",
            "amount_cents": 82_500,
            "method": "card",
            "paid_on": "2026-11-01",
            "reference": "AUTH-991",
            "note": "Prepaid by phone",
        }),
    )
    .await
    .unwrap();
    let decl_id = decl["result"]["declaration"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    commit(
        &db.state,
        &person("alice"),
        "po.confirm_paid",
        json!({ "declaration_id": decl_id }),
    )
    .await
    .unwrap();

    // Later, the invoice arrives (dated after paid_on to also verify paid_on flooring at invoice_date).
    let bill_id = create_and_match_bill(&db, &v, &po_id, "INV-401", 82_500, "2026-11-03").await;
    let detail = get_bill(&db, &bill_id).await;
    assert_eq!(detail["progress"]["status"], "awaiting_release");
    assert_eq!(detail["outside_payment"]["state"], "awaiting_release");

    // Releasing marks it paid with paid_on floored at invoice_date (2026-11-03).
    let rel = confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap();
    assert_eq!(rel["result"]["bill"]["state"], "paid");
    assert_eq!(rel["result"]["bill"]["paid_on"], "2026-11-03");
    assert!(bank.transfers().is_empty());
}

/// Test 5: Declaration arrives after bill is ready_to_pay -> awaiting_release & auto_pay refused.
/// Also: bill already Released to be paid elsewhere is marked paid the moment declaration becomes effective;
/// and a bill with a live bank payment is NOT marked paid by a declaration.
#[tokio::test]
async fn declaration_after_ready_to_pay_and_already_released_and_live_payment() {
    let Some((db, bank, _)) = setup().await else {
        return;
    };
    let v = setup_vendor_with_payee(&db, "Acme Supply", true).await;
    let src = authorize_source(&db, &v, "orders").await;
    let po_id = upsert_active_po(&db, &src, &v, "PO-501", "ext-501", 300_000).await;

    // 5a: Bill is already ready_to_pay at an automatic step.
    let bill_auto = create_and_match_bill(&db, &v, &po_id, "INV-501", 50_000, "2026-11-01").await;
    assert_eq!(
        get_bill(&db, &bill_auto).await["progress"]["status"],
        "ready_to_pay"
    );

    // Declaration arrives -> bill becomes awaiting_release and auto_pay is refused.
    commit(
        &db.state,
        &src,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-501",
            "amount_cents": 50_000,
            "method": "card",
            "paid_on": "2026-11-01",
            "note": "Paid outside after invoice arrived",
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        get_bill(&db, &bill_auto).await["progress"]["status"],
        "awaiting_release"
    );
    let err = commit(
        &db.state,
        &agent("workflow"),
        "bill.auto_pay",
        json!({ "bill_id": bill_auto }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CommandError::Conflict(_)), "{err}");
    assert!(bank.transfers().is_empty());

    // 5b: Bill already Released to be paid elsewhere -> marked paid the moment declaration is confirmed.
    let v_man = setup_vendor_with_payee(&db, "Manual Vendor", false).await;
    let src_man = authorize_source(&db, &v_man, "orders").await;
    let po_man = upsert_active_po(&db, &src_man, &v_man, "PO-502", "ext-502", 100_000).await;
    let bill_rel =
        create_and_match_bill(&db, &v_man, &po_man, "INV-502", 65_000, "2026-11-01").await;
    confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill_rel, "pay_elsewhere": true }),
    )
    .await
    .unwrap();
    assert_eq!(
        get_bill(&db, &bill_rel).await["progress"]["status"],
        "released"
    );

    let d_rel = commit(
        &db.state,
        &src_man,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-502",
            "amount_cents": 65_000,
            "method": "check",
            "paid_on": "2026-11-01",
            "reference": "CHK-502",
            "note": "Check mailed",
        }),
    )
    .await
    .unwrap();
    let d_rel_id = d_rel["result"]["declaration"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(get_bill(&db, &bill_rel).await["bill"]["state"], "open");

    // Confirming immediately settles the already-released bill.
    commit(
        &db.state,
        &person("alice"),
        "po.confirm_paid",
        json!({ "declaration_id": d_rel_id }),
    )
    .await
    .unwrap();
    let settled = get_bill(&db, &bill_rel).await;
    assert_eq!(settled["bill"]["state"], "paid");
    assert_eq!(settled["bill"]["paid_reference"], "CHK-502");
    assert!(bank.transfers().is_empty());

    // 5c: Bill with a live bank payment is NOT marked paid by a declaration.
    let po_live = upsert_active_po(&db, &src_man, &v_man, "PO-503", "ext-503", 100_000).await;
    let bill_live =
        create_and_match_bill(&db, &v_man, &po_live, "INV-503", 75_000, "2026-11-01").await;
    confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill_live }),
    )
    .await
    .unwrap();
    assert_eq!(bank.transfers().len(), 1);

    let d_live = commit(
        &db.state,
        &src_man,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-503",
            "amount_cents": 75_000,
            "method": "card",
            "paid_on": "2026-11-01",
            "note": "Late declaration after bank transfer sent",
        }),
    )
    .await
    .unwrap();
    let d_live_id = d_live["result"]["declaration"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    commit(
        &db.state,
        &person("alice"),
        "po.confirm_paid",
        json!({ "declaration_id": d_live_id }),
    )
    .await
    .unwrap();
    let live_view = get_bill(&db, &bill_live).await;
    assert_eq!(live_view["bill"]["state"], "open");
    assert_eq!(
        live_view["outside_payment"]["state"],
        "live_payment_conflict"
    );
}

/// Test 6: Amount differs by a cent, or two bills of the same amount on the PO:
/// no pairing, automatic payment still blocked, nothing marked paid.
#[tokio::test]
async fn cent_off_or_two_same_amount_bills_never_pair_and_block_auto_pay() {
    let Some((db, bank, _)) = setup().await else {
        return;
    };
    let v = setup_vendor_with_payee(&db, "Acme Supply", true).await;
    let src = authorize_source(&db, &v, "orders").await;

    // 6a: Amount differs by a cent.
    let po_cent = upsert_active_po(&db, &src, &v, "PO-601", "ext-601", 100_000).await;
    let bill_cent = create_and_match_bill(&db, &v, &po_cent, "INV-601", 75_000, "2026-11-01").await;
    let d1 = commit(
        &db.state,
        &src,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-601",
            "amount_cents": 75_001,
            "method": "card",
            "paid_on": "2026-11-01",
            "note": "Off by a cent",
        }),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &person("alice"),
        "po.confirm_paid",
        json!({ "declaration_id": d1["result"]["declaration"]["id"] }),
    )
    .await
    .unwrap();
    let view_cent = get_bill(&db, &bill_cent).await;
    assert_eq!(view_cent["progress"]["status"], "awaiting_release");
    assert_eq!(view_cent["outside_payment"]["state"], "ambiguous");
    assert!(
        commit(
            &db.state,
            &agent("workflow"),
            "bill.auto_pay",
            json!({ "bill_id": bill_cent })
        )
        .await
        .is_err()
    );

    // 6b: Two bills of the same amount on the PO.
    let po_two = upsert_active_po(&db, &src, &v, "PO-602", "ext-602", 200_000).await;
    let b1 = create_and_match_bill(&db, &v, &po_two, "INV-602A", 50_000, "2026-11-01").await;
    let b2 = create_and_match_bill(&db, &v, &po_two, "INV-602B", 50_000, "2026-11-01").await;
    let d2 = commit(
        &db.state,
        &src,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-602",
            "amount_cents": 50_000,
            "method": "card",
            "paid_on": "2026-11-01",
            "note": "One of the two $500 bills",
        }),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &person("alice"),
        "po.confirm_paid",
        json!({ "declaration_id": d2["result"]["declaration"]["id"] }),
    )
    .await
    .unwrap();
    for b in [&b1, &b2] {
        let vw = get_bill(&db, b).await;
        assert_eq!(vw["progress"]["status"], "awaiting_release");
        assert_eq!(vw["outside_payment"]["state"], "ambiguous");
        assert_eq!(vw["bill"]["state"], "open");
    }
    // Releasing an ambiguous bill warns on the preview naming the PO and unmatched declaration.
    let preview = payvy_server::commands::run(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": b1 }),
        payvy_server::commands::Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(
        preview["result"]["outside_declaration_warning"]["po_number"],
        "PO-602"
    );
    assert!(
        preview["result"]["outside_declaration_warning"]["message"]
            .as_str()
            .unwrap()
            .contains("unmatched outside-payment declaration on PO PO-602")
    );
    assert!(bank.transfers().is_empty());
}

/// Test 7: Withdraw: allowed before paid, refused after; bill becomes payable again.
#[tokio::test]
async fn withdraw_before_paid_unblocks_and_refused_after_paid() {
    let Some((db, bank, _)) = setup().await else {
        return;
    };
    let v = setup_vendor_with_payee(&db, "Acme Supply", true).await;
    let src = authorize_source(&db, &v, "orders").await;
    let po_id = upsert_active_po(&db, &src, &v, "PO-701", "ext-701", 100_000).await;
    let bill_id = create_and_match_bill(&db, &v, &po_id, "INV-701", 50_000, "2026-11-01").await;

    let d1 = commit(
        &db.state,
        &src,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-701",
            "amount_cents": 50_000,
            "method": "card",
            "paid_on": "2026-11-01",
            "note": "Mistaken declaration",
        }),
    )
    .await
    .unwrap();
    let d1_id = d1["result"]["declaration"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        get_bill(&db, &bill_id).await["progress"]["status"],
        "awaiting_release"
    );

    // Withdraw before paid -> bill returns to ready_to_pay.
    commit(
        &db.state,
        &person("robert"),
        "po.withdraw_paid",
        json!({ "declaration_id": d1_id, "reason": "wrong PO" }),
    )
    .await
    .unwrap();
    assert_eq!(
        get_bill(&db, &bill_id).await["progress"]["status"],
        "ready_to_pay"
    );
    assert!(get_bill(&db, &bill_id).await["outside_payment"].is_null());

    // Now declare, confirm, release (paid), and verify withdraw is refused.
    let d2 = commit(
        &db.state,
        &src,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-701",
            "amount_cents": 50_000,
            "method": "card",
            "paid_on": "2026-11-01",
            "note": "Real declaration",
        }),
    )
    .await
    .unwrap();
    let d2_id = d2["result"]["declaration"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    commit(
        &db.state,
        &person("alice"),
        "po.confirm_paid",
        json!({ "declaration_id": d2_id }),
    )
    .await
    .unwrap();
    confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap();
    let err = commit(
        &db.state,
        &person("robert"),
        "po.withdraw_paid",
        json!({ "declaration_id": d2_id, "reason": "too late" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CommandError::Conflict(_)), "{err}");
    assert!(bank.transfers().is_empty());

    // After withdrawing a declaration on a manual-release bill, release proceeds
    // and pays normally through the fake bank.
    let v_man = setup_vendor_with_payee(&db, "Withdrawn Pay Vendor", false).await;
    let src_man = authorize_source(&db, &v_man, "orders").await;
    let po_man = upsert_active_po(&db, &src_man, &v_man, "PO-702", "ext-702", 100_000).await;
    let bill_pay =
        create_and_match_bill(&db, &v_man, &po_man, "INV-702", 45_000, "2026-11-01").await;
    let d3 = commit(
        &db.state,
        &src_man,
        "po.declare_paid",
        json!({
            "external_source": "orders",
            "external_id": "ext-702",
            "amount_cents": 45_000,
            "method": "card",
            "paid_on": "2026-11-01",
            "note": "Mistaken declaration to withdraw",
        }),
    )
    .await
    .unwrap();
    let d3_id = d3["result"]["declaration"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        confirm(
            &db.state,
            &person("robert"),
            "bill.release",
            json!({ "bill_id": bill_pay }),
        )
        .await
        .is_err()
    );
    commit(
        &db.state,
        &person("robert"),
        "po.withdraw_paid",
        json!({ "declaration_id": d3_id, "reason": "entered in error" }),
    )
    .await
    .unwrap();
    confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({ "bill_id": bill_pay }),
    )
    .await
    .unwrap();
    assert_eq!(bank.transfers().len(), 1);
}

/// Test 8: Repeat declaration (including without reference) is an error;
/// draft/closed/cancelled PO refused; suspended PO allowed; amount over PO refused;
/// future paid_on refused.
#[tokio::test]
async fn validation_repeat_lifecycle_amount_and_future_date() {
    let Some((db, bank, _)) = setup().await else {
        return;
    };
    let v = setup_vendor_with_payee(&db, "Acme Supply", false).await;

    // Draft PO refused.
    let draft = commit(
        &db.state,
        &person("robert"),
        "po.create",
        json!({ "vendor_id": v, "po_number": "PO-801", "owner": "robert", "authorized_cents": 50_000 }),
    )
    .await
    .unwrap();
    let po_id = draft["result"]["id"].as_str().unwrap().to_owned();
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "po.declare_paid",
            json!({ "po_id": po_id, "amount_cents": 10_000, "method": "cash", "paid_on": "2026-11-01", "note": "draft" }),
        )
        .await
        .is_err()
    );

    // Activate PO.
    commit(
        &db.state,
        &person("robert"),
        "po.activate",
        json!({ "po_id": po_id }),
    )
    .await
    .unwrap();

    // Amount over PO refused.
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "po.declare_paid",
            json!({ "po_id": po_id, "amount_cents": 50_001, "method": "cash", "paid_on": "2026-11-01", "note": "too much" }),
        )
        .await
        .is_err()
    );

    // Future paid_on refused.
    let tomorrow = (*db.clock.lock().unwrap() + chrono::Duration::days(1))
        .date_naive()
        .to_string();
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "po.declare_paid",
            json!({ "po_id": po_id, "amount_cents": 10_000, "method": "cash", "paid_on": tomorrow, "note": "future" }),
        )
        .await
        .is_err()
    );

    // Exact repeat with no reference is refused (IS NOT DISTINCT FROM).
    commit(
        &db.state,
        &person("robert"),
        "po.declare_paid",
        json!({ "po_id": po_id, "amount_cents": 10_000, "method": "cash", "paid_on": "2026-11-01", "note": "first" }),
    )
    .await
    .unwrap();
    let dup = commit(
        &db.state,
        &person("robert"),
        "po.declare_paid",
        json!({ "po_id": po_id, "amount_cents": 10_000, "method": "cash", "paid_on": "2026-11-01", "note": "second" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(dup, CommandError::Conflict(_)), "{dup}");

    // Suspended PO is allowed.
    commit(
        &db.state,
        &person("robert"),
        "po.suspend",
        json!({ "po_id": po_id, "reason": "review" }),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &person("robert"),
        "po.declare_paid",
        json!({ "po_id": po_id, "amount_cents": 12_000, "method": "check", "paid_on": "2026-11-01", "note": "while suspended" }),
    )
    .await
    .unwrap();

    // Closed PO refused.
    commit(
        &db.state,
        &person("robert"),
        "po.close",
        json!({ "po_id": po_id, "reason": "done" }),
    )
    .await
    .unwrap();
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "po.declare_paid",
            json!({ "po_id": po_id, "amount_cents": 5_000, "method": "cash", "paid_on": "2026-11-01", "note": "closed" }),
        )
        .await
        .is_err()
    );
    assert!(bank.transfers().is_empty());
}

/// Test 10: RLS: a second organization sees none of the first org's declarations.
#[tokio::test]
async fn rls_isolates_outside_payments_between_organizations() {
    let Some((db, bank, _)) = setup().await else {
        return;
    };
    let v = setup_vendor_with_payee(&db, "Acme Supply", false).await;
    let created = commit(
        &db.state,
        &person("robert"),
        "po.create",
        json!({ "vendor_id": v, "po_number": "PO-901", "owner": "robert", "authorized_cents": 50_000 }),
    )
    .await
    .unwrap();
    let po_id = created["result"]["id"].as_str().unwrap().to_owned();
    commit(
        &db.state,
        &person("robert"),
        "po.activate",
        json!({ "po_id": po_id }),
    )
    .await
    .unwrap();
    let d = commit(
        &db.state,
        &person("robert"),
        "po.declare_paid",
        json!({ "po_id": po_id, "amount_cents": 25_000, "method": "card", "paid_on": "2026-11-01", "note": "Org 1 only" }),
    )
    .await
    .unwrap();
    let decl_id = d["result"]["declaration"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Create second org and check it cannot see or confirm the declaration.
    let org2 = db.add_org().await;
    let other_admin = Actor::administrator(org2, "other-admin");
    commit(
        &db.state,
        &other_admin,
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
    let err = commit(
        &db.state,
        &other_admin,
        "po.confirm_paid",
        json!({ "declaration_id": decl_id }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CommandError::NotFound(_)), "{err}");

    let mut tx = db.state.store.begin(org2).await.unwrap();
    let rows =
        store::purchase_orders::outside_payments_for_po(&mut tx, Uuid::parse_str(&po_id).unwrap())
            .await
            .unwrap();
    assert!(rows.is_empty());
    assert!(bank.transfers().is_empty());
}
