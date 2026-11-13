//! Phase 1 of the Allied cutover on the Payvy side: connecting VisiBooks
//! per company (sealed key, origin, post_when), and the automatic posts
//! the timing setting drives. The ledger is the fake; the VisiBooks
//! adapter's own endpoints are covered in payvy-ledger's tests.

use payvy_ledger::FakeLedger;
use payvy_server::auth::Auth;
use payvy_server::commands::CommandError;
use payvy_server::mailer::Mailer;
use payvy_server::{Bank, secrets::Sealer};
use payvy_store::{self as store, DEFAULT_ORG, NewLedgerConnection, NewLedgerRecord};
use serde_json::json;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

mod common;
use common::{TestDb, agent, commit, confirm, person};

fn with_fake(db: &TestDb) -> (payvy_server::AppState, FakeLedger) {
    let fake = FakeLedger::new();
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
        .with_ledger(fake.clone())
        .with_auth(Auth::new(
            "https://app.payvy.example",
            Mailer::Memory(Arc::new(Mutex::new(Vec::new()))),
        ));
    (state, fake)
}

/// A connected ledger over the fake adapter, with a vendor and its
/// external id and one expense account in the mirror.
async fn connected(db: &TestDb, state: &payvy_server::AppState, post_when: &str) -> Uuid {
    let vendor = commit(
        state,
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
    let now = *db.clock.lock().unwrap();
    let connection = Uuid::new_v4();
    let sealed = Sealer::from_hex(&"ab".repeat(32))
        .unwrap()
        .seal(
            DEFAULT_ORG,
            &payvy_ledger::seal_purpose("netsuite"),
            "unused",
        )
        .unwrap();
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    store::insert_ledger_connection(
        &mut tx,
        DEFAULT_ORG,
        &NewLedgerConnection {
            id: connection,
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
            post_when,
            ap_account_id: Some("25"),
            defaults: &json!({ "expense_account_id": "65" }),
            bank_account_map: &json!({}),
            created_at: now,
            created_by: "ada",
        },
    )
    .await
    .unwrap();
    store::put_ledger_link(
        &mut tx,
        DEFAULT_ORG,
        &store::PutLedgerLink {
            connection_id: connection,
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
            connection_id: connection,
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
    vendor_id
}

async fn invoice(state: &payvy_server::AppState, vendor: Uuid, number: &str) -> Uuid {
    let created = commit(
        state,
        &person("ada"),
        "bill.create",
        json!({ "vendor_id": vendor, "invoice_number": number,
                "invoice_date": "2027-01-15", "amount_cents": 10_000 }),
    )
    .await
    .unwrap();
    Uuid::parse_str(created["result"]["bill"]["id"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn connect_seals_the_key_and_configure_changes_how_it_posts() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (state, _fake) = with_fake(&db);
    // Agents never connect a ledger.
    let e = commit(
        &state,
        &agent("claude-ap"),
        "ledger.connect",
        json!({ "origin": "https://books.example.test", "entity": "42", "api_key": "vk_live_secret" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    let connected = confirm(
        &state,
        &person("ada"),
        "ledger.connect",
        json!({ "origin": "https://books.example.test", "entity": "42", "api_key": "vk_live_secret", "post_when": "payment" }),
    )
    .await
    .unwrap();
    let text = serde_json::to_string(&connected).unwrap();
    assert!(
        !text.contains("vk_live_secret"),
        "the key never comes back: {text}"
    );
    assert_eq!(connected["result"]["connected"], true);
    assert_eq!(connected["result"]["post_when"], "payment");
    assert_eq!(connected["result"]["origin"], "https://books.example.test");

    // The stored token is sealed, and reconnecting keeps one connection.
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    let row = store::active_ledger_connection(&mut tx)
        .await
        .unwrap()
        .unwrap();
    assert!(!row.token_sealed.windows(13).any(|w| w == b"vk_live_secret"));
    let first_id = row.id;
    tx.rollback().await.unwrap();
    // Rotating the key on the same entity and origin keeps the connection.
    confirm(
        &state,
        &person("ada"),
        "ledger.connect",
        json!({ "origin": "https://books.example.test", "entity": "42", "api_key": "vk_live_secret2" }),
    )
    .await
    .unwrap();
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    assert_eq!(
        store::active_ledger_connection(&mut tx)
            .await
            .unwrap()
            .unwrap()
            .id,
        first_id
    );
    tx.rollback().await.unwrap();
    // A different entity is a different set of books: a new connection,
    // and the old one's links do not carry over.
    confirm(
        &state,
        &person("ada"),
        "ledger.connect",
        json!({ "origin": "https://books.example.test", "entity": "43", "api_key": "vk_live_secret2" }),
    )
    .await
    .unwrap();
    let settings = commit(&state, &agent("claude-ap"), "ledger.settings", json!({}))
        .await
        .unwrap();
    assert_eq!(settings["result"]["entity"], "43");
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    assert_ne!(
        store::active_ledger_connection(&mut tx)
            .await
            .unwrap()
            .unwrap()
            .id,
        first_id
    );
    tx.rollback().await.unwrap();

    // Configure is confirmed (it carries how money is recorded), and
    // asking for what is already set is refused.
    let e = commit(
        &state,
        &person("ada"),
        "ledger.configure",
        json!({ "post_when": "payment" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::NeedsConfirmation(_)), "{e}");
    confirm(
        &state,
        &person("ada"),
        "ledger.configure",
        json!({ "post_when": "payment" }),
    )
    .await
    .unwrap();
    let e = confirm(
        &state,
        &person("ada"),
        "ledger.configure",
        json!({ "post_when": "payment" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn approval_mode_posts_when_the_last_approval_lands() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (state, fake) = with_fake(&db);
    let vendor = connected(&db, &state, "approval").await;
    common::set_workflow(
        &state,
        &person("ada"),
        Some(&vendor.to_string()),
        json!([
            { "id": "one", "type": "approve", "approvers": ["ada"] },
            { "id": "two", "type": "approve", "approvers": ["sam"] },
            { "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let bill = invoice(&state, vendor, "LP-1").await;
    commit(
        &state,
        &person("ada"),
        "bill.approve",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    assert!(
        fake.calls().is_empty(),
        "not yet approved: {:?}",
        fake.calls()
    );
    commit(
        &state,
        &person("sam"),
        "bill.approve",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    // Queued, not posted inside the approval: the worker posts it.
    assert!(fake.calls().is_empty(), "queued only: {:?}", fake.calls());
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    let key = format!("payvy:bill:{bill}");
    assert!(
        fake.calls()
            .iter()
            .any(|c| c == &format!("post_bill {key}")),
        "{:?}",
        fake.calls()
    );
    assert!(
        fake.calls()
            .iter()
            .any(|c| c.starts_with("set_document_link")),
        "the document link follows the post: {:?}",
        fake.calls()
    );
    db.drop().await;
}

#[tokio::test]
async fn payment_mode_posts_the_bill_at_an_outside_payment_and_records_the_unmapped_leg() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (state, fake) = with_fake(&db);
    let vendor = connected(&db, &state, "payment").await;
    common::set_workflow(
        &state,
        &person("ada"),
        Some(&vendor.to_string()),
        json!([{ "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let bill = invoice(&state, vendor, "LP-2").await;
    assert!(fake.calls().is_empty());
    confirm(
        &state,
        &person("ada"),
        "bill.release",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    confirm(
        &state,
        &person("ada"),
        "bill.mark_paid",
        json!({ "bill_id": bill, "paid_on": "2027-01-20", "method": "ach" }),
    )
    .await
    .unwrap();
    // Queued at mark_paid; the worker posts the bill, and the payment
    // leg — with no method mapped to a ledger account — fails on its
    // link where a person can see it, ready to retry once mapped.
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    let key = format!("payvy:bill:{bill}");
    assert!(
        fake.calls()
            .iter()
            .any(|c| c == &format!("post_bill {key}")),
        "{:?}",
        fake.calls()
    );
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    let conn = store::active_ledger_connection(&mut tx)
        .await
        .unwrap()
        .unwrap();
    let link = store::ledger_link(&mut tx, conn.id, "payment", bill)
        .await
        .unwrap()
        .expect("the outside payment leg has a link");
    assert_eq!(link.status, "error");
    tx.rollback().await.unwrap();
    db.drop().await;
}
