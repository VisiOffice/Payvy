mod common;
use chrono::Duration;
use common::*;
use payvy_server::{
    Actor,
    auth::Auth,
    commands::{self, CommandError, Mode},
    mailer::{Mail, Mailer},
    notification_worker,
};
use payvy_store::{self as store, DEFAULT_ORG};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

async fn setup() -> Option<(TestDb, Arc<Mutex<Vec<Mail>>>)> {
    let mut db = TestDb::new().await?;
    let sent = Arc::new(Mutex::new(vec![]));
    db.state = db.state.clone().with_auth(Auth::new(
        "https://payvy.example",
        Mailer::Memory(sent.clone()),
    ));
    for (actor, roles) in [
        ("admin", json!(["admin"])),
        ("alex", json!(["approver"])),
        ("sam", json!(["approver"])),
        ("payer", json!(["payment_releaser"])),
        ("clerk", json!(["ap_clerk"])),
    ] {
        commit(&db.state,&person("operator"),"person.add",json!({"email":format!("{actor}@example.com"),"name":actor,"actor":actor,"roles":roles})).await.unwrap();
    }
    Some((db, sent))
}
async fn enabled(db: &TestDb, on: bool, reminders: bool) {
    commit(
        &db.state,
        &person("operator"),
        "notification.configure",
        json!({"enabled":on,"reminders":reminders}),
    )
    .await
    .unwrap();
}
async fn invoice(db: &TestDb, steps: Value, by: &Actor) -> Uuid {
    let v = vendor(&db.state, &agent("import"), "Notice Vendor").await;
    let w = commit(
        &db.state,
        &agent("import"),
        "workflow.propose",
        json!({"vendor_id":v,"workflow":{"steps":steps}}),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &person("operator"),
        "workflow.accept",
        json!({"proposal_id":w["result"]["proposal_id"]}),
    )
    .await
    .unwrap();
    let b = commit(&db.state,by,"bill.create",json!({"vendor_id":v,"invoice_number":Uuid::new_v4().to_string(),"invoice_date":"2026-11-10","due_date":"2026-12-10","amount_cents":12345})).await.unwrap();
    b["result"]["bill"]["id"].as_str().unwrap().parse().unwrap()
}
fn standard() -> Value {
    json!([{"id":"review","type":"approve","approvers":["alex"]},{"id":"pay","type":"pay","by":["payer"]}])
}
async fn queue(db: &TestDb, bill: Uuid) -> Vec<Uuid> {
    let out = commit(
        &db.state,
        &agent("notifications"),
        "notification.queue",
        json!({"bill_id":bill}),
    )
    .await
    .unwrap();
    serde_json::from_value(out["result"]["queued"].clone()).unwrap()
}
async fn history(db: &TestDb, bill: Uuid) -> Vec<store::Notification> {
    let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::notification_history(&mut tx, bill).await.unwrap()
}
async fn deliver(db: &TestDb, id: Uuid) -> Result<Value, CommandError> {
    commit(
        &db.state,
        &agent("notifications"),
        "notification.deliver",
        json!({"notification_id":id}),
    )
    .await
}

#[tokio::test]
async fn opt_in_preview_and_concurrent_workers_send_once_then_notify_releaser() {
    let Some((db, sent)) = setup().await else {
        return;
    };
    let bill = invoice(&db, standard(), &agent("import")).await;
    assert_eq!(notification_worker::tick(&db.state).await.unwrap(), 0);
    enabled(&db, true, true).await;
    commands::run(
        &db.state,
        &agent("notifications"),
        "notification.queue",
        json!({"bill_id":bill}),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert!(history(&db, bill).await.is_empty());
    let ids = queue(&db, bill).await;
    commands::run(
        &db.state,
        &agent("notifications"),
        "notification.deliver",
        json!({"notification_id":ids[0]}),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert!(sent.lock().unwrap().is_empty());
    assert_eq!(history(&db, bill).await[0].attempts, 0);
    let (a, b) = tokio::join!(
        notification_worker::tick(&db.state),
        notification_worker::tick(&db.state)
    );
    a.unwrap();
    b.unwrap();
    {
        let mail = sent.lock().unwrap();
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].to, "alex@example.com");
        assert!(
            mail[0]
                .text
                .contains(&format!("https://payvy.example/bills/{bill}"))
        );
        assert!(mail[0].text.contains("$123.45"));
        assert!(mail[0].text.contains("Due 2026-12-10"));
    }
    commit(
        &db.state,
        &Actor::person(DEFAULT_ORG, "alex"),
        "bill.approve",
        json!({"bill_id":bill}),
    )
    .await
    .unwrap();
    notification_worker::tick(&db.state).await.unwrap();
    {
        let mail = sent.lock().unwrap();
        assert_eq!(mail.len(), 2);
        assert_eq!(mail[1].to, "payer@example.com");
    }
    confirm(
        &db.state,
        &Actor::person(DEFAULT_ORG, "payer"),
        "bill.release",
        json!({"bill_id":bill,"pay_elsewhere":true}),
    )
    .await
    .unwrap();
    db.advance(10);
    notification_worker::tick(&db.state).await.unwrap();
    assert_eq!(sent.lock().unwrap().len(), 2);
    db.drop().await;
}

#[tokio::test]
async fn approval_reminders_are_spaced_capped_and_stop_after_approval() {
    let Some((db, sent)) = setup().await else {
        return;
    };
    enabled(&db, true, true).await;
    let bill = invoice(&db, standard(), &agent("import")).await;
    notification_worker::tick(&db.state).await.unwrap();
    db.advance(1);
    notification_worker::tick(&db.state).await.unwrap();
    assert_eq!(sent.lock().unwrap().len(), 1);
    for total in 2..=4 {
        db.advance(1);
        notification_worker::tick(&db.state).await.unwrap();
        assert_eq!(sent.lock().unwrap().len(), total);
    }
    db.advance(5);
    notification_worker::tick(&db.state).await.unwrap();
    assert_eq!(sent.lock().unwrap().len(), 4);
    assert_eq!(
        history(&db, bill).await.iter().map(|n| n.reminder).max(),
        Some(3)
    );
    commit(
        &db.state,
        &Actor::person(DEFAULT_ORG, "alex"),
        "bill.approve",
        json!({"bill_id":bill}),
    )
    .await
    .unwrap();
    notification_worker::tick(&db.state).await.unwrap();
    db.advance(9);
    notification_worker::tick(&db.state).await.unwrap();
    assert_eq!(sent.lock().unwrap().len(), 5); // payment notice, no payment reminders
    db.drop().await;
}

#[tokio::test]
async fn rechecks_role_membership_progress_and_reminder_preferences_before_send() {
    let Some((db, sent)) = setup().await else {
        return;
    };
    enabled(&db, true, true).await;
    let bill = invoice(&db, standard(), &agent("import")).await;
    let ids = queue(&db, bill).await;
    commit(
        &db.state,
        &person("operator"),
        "person.set_roles",
        json!({"actor":"alex","roles":["ap_clerk"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        deliver(&db, ids[0]).await.unwrap()["result"]["notification"]["status"],
        "cancelled"
    );
    commit(
        &db.state,
        &person("operator"),
        "person.set_roles",
        json!({"actor":"alex","roles":["approver"]}),
    )
    .await
    .unwrap();
    let bill2 = invoice(&db, standard(), &agent("import")).await;
    let ids = queue(&db, bill2).await;
    commit(
        &db.state,
        &Actor::person(DEFAULT_ORG, "alex"),
        "bill.approve",
        json!({"bill_id":bill2}),
    )
    .await
    .unwrap();
    assert_eq!(
        deliver(&db, ids[0]).await.unwrap()["result"]["notification"]["status"],
        "cancelled"
    );
    let bill3 = invoice(&db, standard(), &agent("import")).await;
    let ids = queue(&db, bill3).await;
    deliver(&db, ids[0]).await.unwrap();
    db.advance(2);
    let reminders = queue(&db, bill3).await;
    enabled(&db, true, false).await;
    assert_eq!(
        deliver(&db, reminders[0]).await.unwrap()["result"]["notification"]["status"],
        "cancelled"
    );
    let bill4 = invoice(&db, standard(), &agent("import")).await;
    let ids = queue(&db, bill4).await;
    commit(
        &db.state,
        &person("operator"),
        "person.remove",
        json!({"actor":"alex"}),
    )
    .await
    .unwrap();
    assert_eq!(
        deliver(&db, ids[0]).await.unwrap()["result"]["notification"]["status"],
        "cancelled"
    );
    assert_eq!(sent.lock().unwrap().len(), 1);
    db.drop().await;
}

#[tokio::test]
async fn named_and_anyone_approvals_obey_roles_and_separation_of_duties() {
    let Some((db, _)) = setup().await else { return };
    enabled(&db, true, true).await;
    commit(
        &db.state,
        &person("operator"),
        "approval_rules.set",
        json!({"submitter_cannot_approve":true}),
    )
    .await
    .unwrap();
    let bill=invoice(&db,json!([{"id":"review","type":"approve","approvers":[]},{"id":"second","type":"approve","approvers":["sam"]},{"id":"pay","type":"pay"}]),&person("admin")).await;
    let ids = queue(&db, bill).await;
    assert_eq!(ids.len(), 1);
    assert_eq!(history(&db, bill).await[0].actor, "alex"); // admin entered it; sam must be saved for the second step; clerk/payer lack approval
    commit(
        &db.state,
        &Actor::person(DEFAULT_ORG, "alex"),
        "bill.approve",
        json!({"bill_id":bill}),
    )
    .await
    .unwrap();
    let ids = queue(&db, bill).await;
    assert_eq!(ids.len(), 1);
    let h = history(&db, bill).await;
    assert!(h.iter().any(|n| n.actor == "sam" && n.step_id == "second"));
    db.drop().await;
}

#[tokio::test]
async fn hold_completion_and_auto_pay_fallback_are_reconciled_without_an_event() {
    let Some((db, sent)) = setup().await else {
        return;
    };
    enabled(&db, true, false).await;
    let bill = invoice(
        &db,
        json!([{"id":"wait","type":"hold","days":2},{"id":"pay","type":"pay","by":["payer"]}]),
        &agent("import"),
    )
    .await;
    notification_worker::tick(&db.state).await.unwrap();
    assert!(sent.lock().unwrap().is_empty());
    db.advance(2);
    notification_worker::tick(&db.state).await.unwrap();
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(history(&db, bill).await[0].action, "release");
    let auto = invoice(
        &db,
        json!([{"id":"pay","type":"pay","release":"auto"}]),
        &agent("import"),
    )
    .await;
    notification_worker::tick(&db.state).await.unwrap();
    assert!(history(&db, auto).await.is_empty());
    // Existing auto-pay fallback state, normally written by the payment worker.
    let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
    sqlx::query("UPDATE bills SET release_by_person=true WHERE id=$1")
        .bind(auto)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    notification_worker::tick(&db.state).await.unwrap();
    assert_eq!(history(&db, auto).await.len(), 2); // admin + payer
    db.drop().await;
}

#[tokio::test]
async fn permissions_tenant_isolation_and_disabled_pending_delivery() {
    let Some((db, sent)) = setup().await else {
        return;
    };
    enabled(&db, true, true).await;
    let bill = invoice(&db, standard(), &agent("import")).await;
    let ids = queue(&db, bill).await;
    for name in [
        "notification.configure",
        "notification.queue",
        "notification.deliver",
    ] {
        assert!(matches!(
            commit(
                &db.state,
                &Actor::person(DEFAULT_ORG, "clerk"),
                name,
                json!({})
            )
            .await,
            Err(CommandError::Forbidden(_))
        ));
    }
    assert!(matches!(
        commit(
            &db.state,
            &agent("bad"),
            "notification.configure",
            json!({"enabled":true,"reminders":true})
        )
        .await,
        Err(CommandError::PersonOnly(_))
    ));
    let other = db.add_org().await;
    assert!(matches!(
        commit(
            &db.state,
            &Actor::administrator(other, "admin"),
            "notification.list",
            json!({"bill_id":bill})
        )
        .await,
        Err(CommandError::NotFound(_))
    ));
    assert!(matches!(
        commit(
            &db.state,
            &Actor::agent(other, "notifications"),
            "notification.deliver",
            json!({"notification_id":ids[0]})
        )
        .await,
        Err(CommandError::NotFound(_))
    ));
    enabled(&db, false, true).await;
    assert!(deliver(&db, ids[0]).await.is_err());
    assert!(sent.lock().unwrap().is_empty());
    enabled(&db, true, true).await;
    deliver(&db, ids[0]).await.unwrap();
    assert_eq!(sent.lock().unwrap().len(), 1);
    db.drop().await;
}

#[tokio::test]
async fn interrupted_claim_becomes_unknown_and_is_never_replayed_or_reminded() {
    let Some((db, sent)) = setup().await else {
        return;
    };
    enabled(&db, true, true).await;
    let bill = invoice(&db, standard(), &agent("import")).await;
    let ids = queue(&db, bill).await;
    let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::claim_notification(&mut tx, ids[0], (db.state.clock)())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(deliver(&db, ids[0]).await.is_err());
    *db.clock.lock().unwrap() += Duration::minutes(6);
    assert_eq!(
        deliver(&db, ids[0]).await.unwrap()["result"]["notification"]["status"],
        "unknown"
    );
    db.advance(20);
    notification_worker::tick(&db.state).await.unwrap();
    assert!(sent.lock().unwrap().is_empty());
    assert_eq!(history(&db, bill).await.len(), 1);
    db.drop().await;
}

#[tokio::test]
async fn provider_rate_limits_retry_but_refusal_and_uncertainty_do_not_leak_or_replay() {
    use axum::{Router, http::StatusCode, routing::post};
    use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
    let Some((mut db, _)) = setup().await else {
        return;
    };
    let status = Arc::new(AtomicU16::new(429));
    let calls = Arc::new(AtomicUsize::new(0));
    let s = status.clone();
    let c = calls.clone();
    let app = Router::new().route(
        "/v2/email/outbound-emails",
        post(move || {
            let s = s.clone();
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::from_u16(s.load(Ordering::SeqCst)).unwrap(),
                    "secret-provider-detail",
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut mailer = Mailer::ses("us-east-1", "fake", "fake", "payvy@example.com").unwrap();
    if let Mailer::Ses { endpoint, .. } = &mut mailer {
        *endpoint = format!("http://{addr}");
    }
    db.state = db
        .state
        .clone()
        .with_auth(Auth::new("https://payvy.example", mailer));
    enabled(&db, true, true).await;
    let bill = invoice(&db, standard(), &agent("import")).await;
    let ids = queue(&db, bill).await;
    let out = deliver(&db, ids[0]).await.unwrap();
    assert_eq!(out["result"]["notification"]["status"], "queued");
    assert!(!out.to_string().contains("secret-provider"));
    assert!(deliver(&db, ids[0]).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    *db.clock.lock().unwrap() += Duration::minutes(2);
    status.store(200, Ordering::SeqCst);
    assert_eq!(
        deliver(&db, ids[0]).await.unwrap()["result"]["notification"]["status"],
        "sent"
    );
    for (code, want) in [(400, "failed"), (500, "unknown")] {
        status.store(code, Ordering::SeqCst);
        let b = invoice(&db, standard(), &agent("import")).await;
        let ids = queue(&db, b).await;
        let out = deliver(&db, ids[0]).await.unwrap();
        assert_eq!(out["result"]["notification"]["status"], want);
        assert!(!out.to_string().contains("secret-provider"));
        assert!(deliver(&db, ids[0]).await.is_err());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    server.abort();
    db.drop().await;
}

#[tokio::test]
async fn slow_delivery_holds_no_bill_or_company_lock_and_does_not_retract_inflight_mail() {
    use axum::{Router, routing::post};
    let Some((mut db, _)) = setup().await else {
        return;
    };
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let a = started.clone();
    let b = release.clone();
    let app = Router::new().route(
        "/v2/email/outbound-emails",
        post(move || {
            let a = a.clone();
            let b = b.clone();
            async move {
                a.notify_one();
                b.notified().await;
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut mailer = Mailer::ses("us-east-1", "fake", "fake", "payvy@example.com").unwrap();
    if let Mailer::Ses { endpoint, .. } = &mut mailer {
        *endpoint = format!("http://{addr}");
    }
    db.state = db
        .state
        .clone()
        .with_auth(Auth::new("https://payvy.example", mailer));
    enabled(&db, true, true).await;
    let bill = invoice(&db, standard(), &agent("import")).await;
    let ids = queue(&db, bill).await;
    let state = db.state.clone();
    let id = ids[0];
    let send = tokio::spawn(async move {
        commit(
            &state,
            &agent("notifications"),
            "notification.deliver",
            json!({"notification_id":id}),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    assert_eq!(history(&db, bill).await[0].status, "sending");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        commit(
            &db.state,
            &Actor::person(DEFAULT_ORG, "alex"),
            "bill.approve",
            json!({"bill_id":bill}),
        )
        .await
        .unwrap();
        enabled(&db, false, true).await;
    })
    .await
    .expect("mail HTTP must not block bill or company actions");
    release.notify_one();
    send.await.unwrap().unwrap();
    assert_eq!(history(&db, bill).await[0].status, "sent");
    server.abort();
    db.drop().await;
}

#[tokio::test]
async fn reconciliation_covers_more_than_one_page_and_drains_backlog() {
    let Some((db, sent)) = setup().await else {
        return;
    };
    enabled(&db, true, false).await;
    for _ in 0..103 {
        invoice(&db, standard(), &agent("import")).await;
    }
    assert_eq!(notification_worker::tick(&db.state).await.unwrap(), 50);
    assert_eq!(notification_worker::tick(&db.state).await.unwrap(), 50);
    assert_eq!(notification_worker::tick(&db.state).await.unwrap(), 3);
    assert_eq!(notification_worker::tick(&db.state).await.unwrap(), 0);
    assert_eq!(sent.lock().unwrap().len(), 103);
    db.drop().await;
}
