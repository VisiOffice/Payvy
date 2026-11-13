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

#[tokio::test]
async fn crashed_claim_is_reclaimed_and_permanent_failures_stop() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (state, fake) = with_fake(&db);
    let vendor = connected(&db, &state, "payment").await;
    common::set_workflow(
        &state,
        &person("ada"),
        Some(&vendor.to_string()),
        json!([
            { "id": "one", "type": "approve", "approvers": ["ada"] },
            { "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let bill = invoice(&state, vendor, "LP-CRASH").await;
    commit(
        &state,
        &person("ada"),
        "bill.approve",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
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
        json!({ "bill_id": bill, "paid_on": "2027-01-20", "method": "ach", "reference": "ach-1" }),
    )
    .await
    .unwrap();

    // A worker claims the job and dies: the job stays `sending`.
    let now = *db.clock.lock().unwrap();
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    let claimed = store::claim_ledger_jobs(&mut tx, now, 10).await.unwrap();
    assert_eq!(claimed.len(), 2, "the bill post and its payment leg");
    tx.commit().await.unwrap();
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    assert!(
        fake.calls().is_empty(),
        "the lease is still held: {:?}",
        fake.calls()
    );

    // The lease expires; the next tick reclaims and posts.
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    sqlx::query("UPDATE ledger_jobs SET lease_until = $1")
        .bind(now - chrono::Duration::hours(1))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    assert!(
        fake.calls().iter().any(|c| c.starts_with("post_bill")),
        "reclaimed and posted: {:?}",
        fake.calls()
    );

    // A job that keeps failing stops after the cap and reads as dead.
    let ghost = Uuid::new_v4();
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    store::enqueue_ledger_job(
        &mut tx,
        DEFAULT_ORG,
        "bank_reference",
        None,
        Some(ghost),
        &json!({}),
        now,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for hours in 1..=9 {
        let at = now + chrono::Duration::hours(hours);
        let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
        let jobs = store::claim_ledger_jobs(&mut tx, at, 10).await.unwrap();
        for job in &jobs {
            store::fail_ledger_job(&mut tx, job.id, "boom", at, false)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
    }
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM ledger_jobs WHERE payment_id = $1")
        .bind(ghost)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(status, "dead");

    // But a transient failure (an outage) never goes dead, however
    // long it lasts: the job posts when the ledger comes back.
    let ghost2 = Uuid::new_v4();
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    store::enqueue_ledger_job(
        &mut tx,
        DEFAULT_ORG,
        "bank_reference",
        None,
        Some(ghost2),
        &json!({}),
        now,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for hours in 1..=12 {
        let at = now + chrono::Duration::hours(hours);
        let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
        let jobs = store::claim_ledger_jobs(&mut tx, at, 10).await.unwrap();
        for job in &jobs {
            store::fail_ledger_job(&mut tx, job.id, "timeout", at, true)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
    }
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM ledger_jobs WHERE payment_id = $1")
        .bind(ghost2)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(status, "failed", "an outage must not kill the job");
    db.drop().await;
}

#[tokio::test]
async fn unfinished_workflow_ends_the_job_until_the_last_step_lands() {
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
            { "id": "wait", "type": "wait_for", "signal": "funded" },
            { "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let bill = invoice(&state, vendor, "LP-SIGNAL").await;
    commit(
        &state,
        &person("ada"),
        "bill.approve",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    // The approval queued a job, but the workflow still waits on a
    // signal: the job ends instead of deferring forever, and posts
    // nothing.
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    assert!(
        !fake.calls().iter().any(|c| c.starts_with("post_bill")),
        "waiting on a signal posts nothing yet: {:?}",
        fake.calls()
    );
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger_jobs WHERE bill_id = $1 AND status IN ('pending', 'sending', 'failed')",
    )
    .bind(bill)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(live, 0, "the job ended rather than deferring forever");

    // The signal finishes the workflow and queues a fresh job.
    commit(
        &state,
        &person("ada"),
        "bill.signal",
        json!({ "bill_id": bill, "signal": "funded" }),
    )
    .await
    .unwrap();
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    assert!(
        fake.calls().iter().any(|c| c.starts_with("post_bill")),
        "the signal's job posts: {:?}",
        fake.calls()
    );
    db.drop().await;
}

#[tokio::test]
async fn held_bill_posts_when_the_hold_releases() {
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
            { "id": "cool", "type": "hold", "days": 3 },
            { "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let bill = invoice(&state, vendor, "LP-HOLD").await;
    commit(
        &state,
        &person("ada"),
        "bill.approve",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    assert!(
        !fake.calls().iter().any(|c| c.starts_with("post_bill")),
        "held bills wait: {:?}",
        fake.calls()
    );
    *db.clock.lock().unwrap() += chrono::Duration::days(4);
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    assert!(
        fake.calls().iter().any(|c| c.starts_with("post_bill")),
        "posts once the hold releases: {:?}",
        fake.calls()
    );
    db.drop().await;
}

#[tokio::test]
async fn an_outage_does_not_kill_the_job() {
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
            { "id": "pay", "type": "pay", "release": "manual" }]),
    )
    .await;
    let bill = invoice(&state, vendor, "LP-OUTAGE").await;
    commit(
        &state,
        &person("ada"),
        "bill.approve",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    // The ledger is down for longer than the old eight-attempt cap
    // would have survived (about two hours of backoff).
    for _ in 0..10 {
        fake.script_err(payvy_ledger::LedgerError::new(
            payvy_ledger::Code::Timeout,
            "the ledger did not answer",
        ));
    }
    for _ in 0..10 {
        payvy_server::ledger_worker::tick(&state).await.unwrap();
        *db.clock.lock().unwrap() += chrono::Duration::hours(2);
    }
    // The fake records attempts too: ten tries, none of them a post
    // that stuck (the job is still live, not dead).
    let attempts = fake
        .calls()
        .iter()
        .filter(|c| c.starts_with("post_bill"))
        .count();
    assert_eq!(attempts, 10, "ten failed tries: {:?}", fake.calls());
    // The ledger comes back; the same job posts.
    payvy_server::ledger_worker::tick(&state).await.unwrap();
    let calls = fake
        .calls()
        .iter()
        .filter(|c| c.starts_with("post_bill"))
        .count();
    assert_eq!(
        calls,
        11,
        "posted once the ledger returned: {:?}",
        fake.calls()
    );
    db.drop().await;
}
