mod common;
use common::*;
use payvy_domain::{
    commands::COMMANDS,
    permissions::{Permission, Role},
};
use payvy_server::{
    Actor,
    commands::{self, CommandError, Mode},
};
use payvy_store::{self as store, DEFAULT_ORG};
use serde_json::{Value, json};

fn member(name: &str) -> Actor {
    Actor::person(DEFAULT_ORG, name)
}
async fn add(db: &TestDb, name: &str, roles: Value) {
    commit(
        &db.state,
        &person("operator"),
        "person.add",
        json!({"name":name,"actor":name,"email":format!("{name}@example.com"),"roles":roles}),
    )
    .await
    .unwrap();
}
async fn assign(db: &TestDb, name: &str, roles: Value) -> Result<Value, CommandError> {
    commit(
        &db.state,
        &person("operator"),
        "person.set_roles",
        json!({"actor":name,"roles":roles}),
    )
    .await
}

#[tokio::test]
async fn every_registry_command_enforces_the_role_in_preview_and_commit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    for (name, role) in [
        ("clerk", Role::ApClerk),
        ("approver", Role::Approver),
        ("payer", Role::PaymentReleaser),
    ] {
        add(&db, name, json!([role])).await;
        for spec in COMMANDS {
            if spec.permission.allows(&[role]) {
                continue;
            }
            let denied = commands::run(
                &db.state,
                &member(name),
                spec.name,
                json!({}),
                Mode::Preview,
            )
            .await;
            assert!(
                matches!(denied, Err(CommandError::Forbidden(_))),
                "{}: {:?}",
                spec.name,
                denied
            );
            assert!(matches!(
                commit(&db.state, &member(name), spec.name, json!({})).await,
                Err(CommandError::Forbidden(_))
            ));
        }
    }
    // Naming yourself admin is not an authorization credential.
    assert!(matches!(
        commit(&db.state, &member("admin"), "person.list", json!({})).await,
        Err(CommandError::Forbidden(_))
    ));
    db.drop().await;
}

#[tokio::test]
async fn new_people_default_to_clerk_and_roles_are_audited_and_previewable() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let p = commit(
        &db.state,
        &person("operator"),
        "person.add",
        json!({"name":"Clerk","email":"clerk@example.com"}),
    )
    .await
    .unwrap();
    assert_eq!(p["result"]["person"]["roles"], json!(["ap_clerk"]));
    let input = json!({"actor":"clerk","roles":["approver","payment_releaser"]});
    commands::run(
        &db.state,
        &person("operator"),
        "person.set_roles",
        input.clone(),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert!(matches!(
        commit(&db.state, &member("clerk"), "bill.approve", json!({})).await,
        Err(CommandError::Forbidden(_))
    ));
    let changed = commit(
        &db.state,
        &person("operator"),
        "person.set_roles",
        input.clone(),
    )
    .await
    .unwrap();
    assert_eq!(changed["events"], json!(["person.roles_changed"]));
    assert!(matches!(
        commit(&db.state, &person("operator"), "person.set_roles", input).await,
        Err(CommandError::Conflict(_))
    ));
    let events = commit(
        &db.state,
        &person("operator"),
        "event.list",
        json!({"entity_id":p["result"]["person"]["id"]}),
    )
    .await
    .unwrap();
    let event = events["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "person.roles_changed")
        .unwrap();
    assert_eq!(event["payload"]["from"], json!(["ap_clerk"]));
    assert_eq!(
        event["payload"]["to"],
        json!(["approver", "payment_releaser"])
    );
    for roles in [json!([]), json!(["admin", "admin"]), json!(["owner"])] {
        assert!(assign(&db, "clerk", roles).await.is_err());
    }
    db.drop().await;
}

#[tokio::test]
async fn roles_do_not_bypass_named_workflows_and_revocation_invalidates_payment_commit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    add(&db, "clerk", json!(["ap_clerk"])).await;
    add(&db, "ann", json!(["approver"])).await;
    add(&db, "payer", json!(["payment_releaser"])).await;
    add(&db, "other", json!(["admin"])).await;
    let v = vendor(&db.state, &member("clerk"), "Role test").await;
    set_workflow(
        &db.state,
        &person("operator"),
        Some(&v),
        json!([
            {"id":"approve","type":"approve","approvers":["ann"]},
            {"id":"pay","type":"pay","by":["payer"]}
        ]),
    )
    .await;
    let b = bill(&db.state, &member("clerk"), &v, "R-1", 100).await;
    let input = json!({"bill_id":b});
    assert!(matches!(
        commit(&db.state, &member("other"), "bill.approve", input.clone()).await,
        Err(CommandError::Forbidden(_))
    ));
    let inbox = commit(&db.state, &member("clerk"), "inbox.get", json!({}))
        .await
        .unwrap();
    assert_eq!(inbox["result"]["count"], 0);
    commit(&db.state, &member("ann"), "bill.approve", input.clone())
        .await
        .unwrap();
    let detail = commit(&db.state, &member("ann"), "bill.get", input.clone())
        .await
        .unwrap();
    assert_eq!(detail["result"]["can_release"], false);
    let pay = json!({"bill_id":b,"pay_elsewhere":true});
    assert!(
        commands::run(
            &db.state,
            &member("other"),
            "bill.release",
            pay.clone(),
            Mode::Preview
        )
        .await
        .is_err()
    );
    let preview = commands::run(
        &db.state,
        &member("payer"),
        "bill.release",
        pay.clone(),
        Mode::Preview,
    )
    .await
    .unwrap();
    assign(&db, "payer", json!(["ap_clerk"])).await.unwrap();
    let result = commands::run(
        &db.state,
        &member("payer"),
        "bill.release",
        pay,
        Mode::Commit {
            confirm_token: Some(preview["confirm_token"].as_str().unwrap().into()),
        },
    )
    .await;
    assert!(matches!(result, Err(CommandError::Forbidden(_))));
    assert_eq!(
        status(&db.state, &member("payer"), &b).await,
        "awaiting_release"
    );
    db.drop().await;
}

#[tokio::test]
async fn concurrent_admin_changes_cannot_leave_no_admin_or_use_revoked_authority() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    add(&db, "ann", json!(["admin"])).await;
    add(&db, "bob", json!(["admin"])).await;
    let ann = member("ann");
    let bob = member("bob");
    let (a, b) = tokio::join!(
        commit(
            &db.state,
            &ann,
            "person.set_roles",
            json!({"actor":"bob","roles":["ap_clerk"]})
        ),
        commit(
            &db.state,
            &bob,
            "person.set_roles",
            json!({"actor":"ann","roles":["ap_clerk"]})
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
    let admins = store::sign_in_admins(&mut tx, DEFAULT_ORG).await.unwrap();
    assert_eq!(admins.len(), 1);
    tx.rollback().await.unwrap();
    assert!(matches!(
        assign(&db, &admins[0], json!(["ap_clerk"])).await,
        Err(CommandError::Conflict(_))
    ));
    assert!(matches!(
        commit(
            &db.state,
            &person("operator"),
            "person.remove",
            json!({"actor":admins[0]})
        )
        .await,
        Err(CommandError::Conflict(_))
    ));
    db.drop().await;
}

#[tokio::test]
async fn roles_are_company_scoped_and_removed_people_lose_keys_and_grants() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    add(&db, "ann", json!(["admin"])).await;
    add(&db, "bob", json!(["admin"])).await;
    let key = "person-bob-test";
    db.state
        .store
        .insert_api_key(
            DEFAULT_ORG,
            "bob",
            store::ActorKind::Person,
            &payvy_server::hash_key(key),
        )
        .await
        .unwrap();
    let other = db.add_org().await;
    assert!(matches!(
        commit(
            &db.state,
            &Actor::person(other, "ann"),
            "person.list",
            json!({})
        )
        .await,
        Err(CommandError::Forbidden(_))
    ));
    commit(
        &db.state,
        &member("ann"),
        "person.remove",
        json!({"actor":"bob"}),
    )
    .await
    .unwrap();
    assert!(
        db.state
            .store
            .actor_for_key_hash(&payvy_server::hash_key(key))
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        commit(&db.state, &member("bob"), "vendor.list", json!({})).await,
        Err(CommandError::Forbidden(_))
    ));
    add(&db, "bob", json!(["ap_clerk"])).await;
    assert!(
        db.state
            .store
            .actor_for_key_hash(&payvy_server::hash_key(key))
            .await
            .unwrap()
            .is_none()
    );
    assert!(!Permission::Admin.allows(&[Role::ApClerk]));
    db.drop().await;
}

#[tokio::test]
async fn existing_sessions_and_person_keys_follow_role_changes_and_mcp_filters_tools() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use chrono::Duration;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    async fn http(db: &TestDb, path: &str, body: Option<Value>, key: bool) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(path)
            .header("content-type", "application/json");
        req = if key {
            req.header("authorization", "Bearer role-key")
        } else {
            req.header("cookie", "payvy_session=role-session")
                .header("x-payvy-client", "web")
        };
        let response = payvy_server::router(db.state.clone())
            .oneshot(
                req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    let Some(db) = TestDb::new().await else {
        return;
    };
    add(&db, "clerk", json!(["ap_clerk"])).await;
    let people = commit(&db.state, &person("operator"), "person.list", json!({}))
        .await
        .unwrap();
    let id = people["result"]["people"][0]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let now = *db.clock.lock().unwrap();
    db.state
        .store
        .insert_sign_in_link("role-link", id, now, now + Duration::minutes(5))
        .await
        .unwrap();
    db.state
        .store
        .redeem_sign_in_link(
            "role-link",
            &payvy_server::hash_key("role-session"),
            now,
            now + Duration::days(1),
        )
        .await
        .unwrap()
        .unwrap();
    db.state
        .store
        .insert_api_key(
            DEFAULT_ORG,
            "clerk",
            store::ActorKind::Person,
            &payvy_server::hash_key("role-key"),
        )
        .await
        .unwrap();
    for key in [false, true] {
        let (status, _) = http(
            &db,
            "/api/commands/person.set_roles",
            Some(json!({"mode":"commit","input":{"actor":"clerk","roles":["admin"]}})),
            key,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (_, listed) = http(
            &db,
            "/mcp",
            Some(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})),
            key,
        )
        .await;
        let names: Vec<_> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"bill_create"));
        assert!(!names.contains(&"bill_approve"));
        assert!(!names.contains(&"person_set_roles"));
    }
    assign(&db, "clerk", json!(["approver"])).await.unwrap();
    let (_, me) = http(&db, "/auth/session", None, false).await;
    assert_eq!(me["roles"], json!(["approver"]));
    assert!(
        me["commands"]
            .as_array()
            .unwrap()
            .contains(&json!("bill.approve"))
    );
    assert!(
        !me["commands"]
            .as_array()
            .unwrap()
            .contains(&json!("bill.create"))
    );
    for key in [false, true] {
        let (status, _) = http(
            &db,
            "/api/commands/bill.create",
            Some(json!({"mode":"commit","input":{}})),
            key,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (_, listed) = http(&db, "/api/commands", None, key).await;
        assert!(
            listed["commands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == "bill.approve")
        );
        assert!(
            !listed["commands"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == "bill.create")
        );
    }
    db.drop().await;
}

#[tokio::test]
async fn migration_preserves_existing_people_and_keys_without_restoring_removed_access() {
    use sqlx::Executor;
    let Some(db) = TestDb::new().await else {
        return;
    };
    add(&db, "existing", json!(["ap_clerk"])).await;
    add(&db, "removed", json!(["ap_clerk"])).await;
    for name in ["legacy_key_only", "removed"] {
        db.state
            .store
            .insert_api_key(
                DEFAULT_ORG,
                name,
                store::ActorKind::Person,
                &payvy_server::hash_key(name),
            )
            .await
            .unwrap();
    }
    commit(
        &db.state,
        &person("operator"),
        "person.remove",
        json!({"actor":"removed"}),
    )
    .await
    .unwrap();
    // Simulate a key left active by the pre-roles removal implementation.
    let forgotten_key = payvy_server::hash_key("forgotten-removed-person-key");
    db.state
        .store
        .insert_api_key(
            DEFAULT_ORG,
            "removed",
            store::ActorKind::Person,
            &forgotten_key,
        )
        .await
        .unwrap();
    // Run the actual migration's backfill and RLS against existing identities,
    // using a separate table so the harness's already-applied migration stays intact.
    let migration = include_str!("../../payvy-store/migrations/0017_roles.sql")
        .replace("actor_roles", "role_migration_probe");
    let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
    tx.execute(sqlx::AssertSqlSafe(migration)).await.unwrap();
    let rows: Vec<(String, Vec<String>)> =
        sqlx::query_as("SELECT actor, roles FROM role_migration_probe ORDER BY actor")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            ("existing".into(), vec!["admin".into()]),
            ("legacy_key_only".into(), vec!["admin".into()])
        ]
    );
    tx.commit().await.unwrap();
    assert!(
        db.state
            .store
            .actor_for_key_hash(&forgotten_key)
            .await
            .unwrap()
            .is_none()
    );
    let other = db.add_org().await;
    let mut tx = db.state.store.begin(other).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM role_migration_probe")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
    tx.rollback().await.unwrap();
    db.drop().await;
}
