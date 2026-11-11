//! Settings > People and Company: who can sign in, and the organization's name.

use payvy_server::Actor;
use payvy_server::commands::CommandError;
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, commit, person};

async fn add(db: &TestDb, by: &Actor, email: &str, name: &str) -> Value {
    commit(
        &db.state,
        by,
        "person.add",
        json!({ "email": email, "name": name }),
    )
    .await
    .unwrap()
}

async fn actors(db: &TestDb) -> Vec<String> {
    let r = commit(&db.state, &agent("claude-ap"), "person.list", json!({}))
        .await
        .unwrap();
    r["result"]["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["actor"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_person_renames_the_organization_and_sign_in_shows_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let got = commit(&db.state, &agent("claude-ap"), "org.get", json!({}))
        .await
        .unwrap();
    assert_eq!(got["result"]["organization"]["name"], "Default");
    assert_eq!(got["events"], json!([]));

    let r = commit(
        &db.state,
        &person("robert"),
        "org.update",
        json!({ "name": "  Acme Holdings  " }),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["organization"]["name"], "Acme Holdings");
    assert_eq!(r["events"], json!(["org.renamed"]));

    let org_id = r["result"]["organization"]["id"].as_str().unwrap();
    let events = commit(
        &db.state,
        &agent("claude-ap"),
        "event.list",
        json!({ "entity_id": org_id }),
    )
    .await
    .unwrap();
    let e = &events["result"]["events"][0];
    assert_eq!(e["kind"], "org.renamed");
    assert_eq!(e["actor"], "robert");
    assert_eq!(
        e["payload"],
        json!({ "from": "Default", "to": "Acme Holdings" })
    );

    // Sign-in reads the same row, so the app's shell and the emailed
    // link both carry the new name.
    add(&db, &person("robert"), "robert@example.com", "Robert").await;
    let signed_in = db
        .state
        .store
        .people_by_email("robert@example.com")
        .await
        .unwrap();
    assert_eq!(signed_in[0].org_name, "Acme Holdings");
    db.drop().await;
}

#[tokio::test]
async fn renaming_checks_the_name_and_who_asks() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let rename = |name: Value, who: Actor| {
        let state = db.state.clone();
        async move { commit(&state, &who, "org.update", json!({ "name": name })).await }
    };

    for bad in [
        json!(""),
        json!("   "),
        json!("Acme\nCo"),
        json!("a".repeat(101)),
    ] {
        let e = rename(bad.clone(), person("robert")).await.unwrap_err();
        assert!(matches!(e, CommandError::Invalid(_)), "{bad}: {e}");
    }
    // 100 characters is fine, counted as characters rather than bytes.
    rename(json!("é".repeat(100)), person("robert"))
        .await
        .unwrap();

    // A rename that changes nothing is an error, not a quiet success.
    let e = rename(json!(" ".to_owned() + &"é".repeat(100)), person("robert"))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    let e = rename(json!("Agent Co"), agent("claude-ap"))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    let e = commit(
        &db.state,
        &person("robert"),
        "org.update",
        json!({ "name": "X", "id": "other" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn renaming_touches_only_your_own_organization() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let other = db.add_org().await;
    commit(
        &db.state,
        &Actor::administrator(other, "olga"),
        "org.update",
        json!({ "name": "Other Renamed" }),
    )
    .await
    .unwrap();
    let mine = commit(&db.state, &person("robert"), "org.get", json!({}))
        .await
        .unwrap();
    assert_eq!(mine["result"]["organization"]["name"], "Default");
    let theirs = commit(&db.state, &Actor::agent(other, "a"), "org.get", json!({}))
        .await
        .unwrap();
    assert_eq!(theirs["result"]["organization"]["name"], "Other Renamed");
    db.drop().await;
}

#[tokio::test]
async fn people_list_says_who_added_whom_and_when() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    add(&db, &person("admin"), "robert@example.com", "Robert").await;
    db.advance(1);
    let r = add(&db, &person("robert"), "dana@example.com", "Dana").await;
    assert!(
        r["result"]["next"]
            .as_str()
            .unwrap()
            .contains("sign-in link")
    );
    let list = commit(&db.state, &agent("claude-ap"), "person.list", json!({}))
        .await
        .unwrap();
    let people = list["result"]["people"].as_array().unwrap();
    assert_eq!(people.len(), 2);
    assert_eq!(people[0]["name"], "Dana");
    assert_eq!(people[0]["created_by"], "robert");
    assert_eq!(people[1]["created_by"], "admin");
    assert!(people[0]["created_at"].as_str().unwrap() > people[1]["created_at"].as_str().unwrap());
    db.drop().await;
}

#[tokio::test]
async fn nobody_removes_themselves_or_the_last_person() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    add(&db, &person("admin"), "robert@example.com", "Robert").await;

    let remove = |actor: &'static str, who: Actor| {
        let state = db.state.clone();
        async move { commit(&state, &who, "person.remove", json!({ "actor": actor })).await }
    };

    // The last person, even asked by someone else (a person key).
    let e = remove("robert", person("admin")).await.unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    add(&db, &person("robert"), "dana@example.com", "Dana").await;

    // Yourself: someone else has to.
    let e = remove("robert", person("robert")).await.unwrap_err();
    assert!(matches!(e, CommandError::Forbidden(_)), "{e}");
    let e = remove(" Robert ", person("robert")).await.unwrap_err();
    assert!(matches!(e, CommandError::Forbidden(_)), "{e}");

    // Agents never remove people.
    let e = remove("dana", agent("claude-ap")).await.unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    let e = remove("nobody", person("robert")).await.unwrap_err();
    assert!(matches!(e, CommandError::NotFound(_)), "{e}");

    let r = remove("dana", person("robert")).await.unwrap();
    assert_eq!(r["events"], json!(["person.removed"]));
    assert_eq!(actors(&db).await, ["robert"]);

    // Removing someone twice is not a second success.
    let e = remove("dana", person("robert")).await.unwrap_err();
    assert!(matches!(e, CommandError::NotFound(_)), "{e}");

    // Dana can be added back, and then removed by Robert again.
    add(&db, &person("robert"), "dana@example.com", "Dana").await;
    remove("dana", person("robert")).await.unwrap();
    db.drop().await;
}
