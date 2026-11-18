mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use common::*;
use http_body_util::BodyExt;
use payvy_server::commands::CommandError;
use payvy_store::DEFAULT_ORG;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(db: &TestDb, key: &str, name: &str, input: Value) -> (StatusCode, Value) {
    let response = payvy_server::router(db.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/commands/{name}"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::from(json!({ "input": input }).to_string()))
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

#[tokio::test]
async fn an_admin_makes_an_agent_key_that_works_until_revoked() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let admin = person("robert");
    let made = commit(
        &db.state,
        &admin,
        "agent_key.create",
        json!({"name":" Quarry-Agent "}),
    )
    .await
    .unwrap();
    let key = made["result"]["key"].as_str().unwrap().to_owned();
    let id = made["result"]["id"].as_str().unwrap().to_owned();
    assert!(key.starts_with("pv_"));
    assert_eq!(made["result"]["actor"], "quarry-agent");

    // The key is in the result once; not in the list, not in the event.
    let listed = commit(&db.state, &admin, "agent_key.list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["result"]["keys"][0]["id"], id.as_str());
    assert!(listed["result"]["keys"][0]["revoked_at"].is_null());
    assert!(!listed.to_string().contains(&key));
    let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
    let events = payvy_store::list_events(&mut tx, None, 50).await.unwrap();
    tx.rollback().await.unwrap();
    let created = serde_json::to_string(&events).unwrap();
    assert!(created.contains("agent_key.created"));
    assert!(!created.contains(&key));

    // The agent works under its name and can't sign off or mint keys.
    let (status, body) = call(
        &db,
        &key,
        "vendor.create",
        json!({"name":"Allied Storage Containers"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = call(&db, &key, "agent_key.create", json!({"name":"another"})).await;
    assert_ne!(status, StatusCode::OK);
    let (status, body) = call(&db, &key, "agent_key.list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.to_string().contains(&key));

    // Revoked: the next call is refused; revoking again is a conflict.
    commit(&db.state, &admin, "agent_key.revoke", json!({"id":id}))
        .await
        .unwrap();
    let (status, _) = call(&db, &key, "vendor.list", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(matches!(
        commit(&db.state, &admin, "agent_key.revoke", json!({"id":id})).await,
        Err(CommandError::Conflict(_))
    ));
    db.drop().await;
}

#[tokio::test]
async fn agent_names_stay_apart_from_people_and_payvys_workers() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let admin = person("robert");
    commit(
        &db.state,
        &admin,
        "person.add",
        json!({"name":"Robert","actor":"robert","email":"robert@example.com","roles":["admin"]}),
    )
    .await
    .unwrap();
    for (name, conflict) in [
        ("robert", true),
        ("email", false),
        ("workflow", false),
        ("Bad Name", false),
        ("", false),
    ] {
        let r = commit(&db.state, &admin, "agent_key.create", json!({"name":name})).await;
        if conflict {
            assert!(matches!(r, Err(CommandError::Conflict(_))), "{name}: {r:?}");
        } else {
            assert!(matches!(r, Err(CommandError::Invalid(_))), "{name}: {r:?}");
        }
    }
    // A revoke from another company's key id is not found.
    let other = db.add_org().await;
    let admin_elsewhere = payvy_server::Actor::administrator(other, "ann");
    let made = commit(
        &db.state,
        &admin,
        "agent_key.create",
        json!({"name":"helper"}),
    )
    .await
    .unwrap();
    assert!(matches!(
        commit(
            &db.state,
            &admin_elsewhere,
            "agent_key.revoke",
            json!({"id": made["result"]["id"]})
        )
        .await,
        Err(CommandError::NotFound(_))
    ));
    // An agent can't make or revoke keys.
    let refused = commit(
        &db.state,
        &agent("helper"),
        "agent_key.create",
        json!({"name":"more"}),
    )
    .await;
    assert!(matches!(refused, Err(CommandError::PersonOnly(_))));
    db.drop().await;
}
