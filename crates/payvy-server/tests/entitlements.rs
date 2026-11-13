//! Plan entitlements (docs/hosted.md): only the operator sets them, versions
//! only go up, each company has its own, and hosted mode refuses what a plan
//! doesn't include before anything else runs. Payments under a plan are in
//! `payments.rs`; the reader under a plan is in `extraction.rs`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use payvy_server::Actor;
use payvy_server::commands::CommandError;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

mod common;
use common::{ADMIN_KEY, OPERATOR_KEY, TestDb, commit, entitle, hosted};

async fn post(
    state: &payvy_server::AppState,
    path: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = payvy_server::router(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn only_the_operator_sets_a_plan() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let state = hosted(&db.state);
    let org = payvy_store::DEFAULT_ORG;
    let input =
        json!({ "input": { "org_id": org, "reader": true, "payments": true, "version": 1 } });

    // A company's own keys can't reach it, on either surface.
    let (status, _) = post(
        &state,
        "/operator/commands/entitlement.set",
        ADMIN_KEY,
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = post(
        &state,
        "/api/commands/entitlement.set",
        ADMIN_KEY,
        json!({ "input": input["input"] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = post(
        &state,
        "/operator/commands/entitlement.set",
        OPERATOR_KEY,
        input,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["changed"], true);
    assert_eq!(body["result"]["in_force"]["payments"], true);

    let (status, body) = post(
        &state,
        "/operator/commands/entitlement.get",
        OPERATOR_KEY,
        json!({ "input": { "org_id": org } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["entitlement"]["version"], 1);

    // Not a company, and not a version.
    let e = entitle(&state, Uuid::new_v4(), true, true, 1)
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::NotFound(_)), "{e}");
    let e = entitle(&state, org, true, true, 0).await.unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn each_company_has_its_own_plan() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let state = hosted(&db.state);
    let other = db.add_org().await;
    entitle(&state, payvy_store::DEFAULT_ORG, true, true, 1)
        .await
        .unwrap();

    let mine = commit(&state, &common::person("robert"), "org.get", json!({}))
        .await
        .unwrap();
    assert_eq!(mine["result"]["plan"]["payments"], true);
    let theirs = commit(
        &state,
        &Actor::administrator(other, "ops"),
        "org.get",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        theirs["result"]["plan"],
        json!({ "hosted": true, "provisional": true, "reader": false, "payments": false })
    );

    // The other company's version history is its own.
    entitle(&state, other, false, true, 1).await.unwrap();
    let e = entitle(&state, payvy_store::DEFAULT_ORG, false, false, 1)
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn the_plan_is_checked_before_anything_else() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let state = hosted(&db.state);
    // No storage and no model here: the plan answers first, so a
    // bring-your-own-agent company is told why, not that a model is missing.
    let e = commit(
        &state,
        &common::agent("claude-ap"),
        "document.extract",
        json!({ "document_id": Uuid::new_v4() }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::NotEntitled(_)), "{e}");
    assert_eq!(e.status(), StatusCode::FORBIDDEN);

    // Self-hosted, the same call gets as far as asking for a model.
    let e = commit(
        &db.state,
        &common::agent("claude-ap"),
        "document.extract",
        json!({ "document_id": Uuid::new_v4() }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::NotConfigured(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn concurrent_sets_of_one_version_store_one() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let state = hosted(&db.state);
    let org = payvy_store::DEFAULT_ORG;
    let (a, b) = tokio::join!(
        entitle(&state, org, true, true, 5),
        entitle(&state, org, false, true, 5)
    );
    // One wins; the other is refused as not newer, never silently merged.
    let wins = [&a, &b].iter().filter(|r| r.is_ok()).count();
    assert_eq!(wins, 1, "{a:?} {b:?}");
    let events = commit(
        &state,
        &common::person("robert"),
        "event.list",
        json!({ "entity_id": org }),
    )
    .await
    .unwrap()["result"]
        .to_string();
    assert_eq!(events.matches("entitlement.set").count(), 1, "{events}");
    db.drop().await;
}
