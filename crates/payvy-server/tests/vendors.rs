//! Editing vendors against a real database.

use payvy_server::commands::CommandError;
use serde_json::json;

mod common;
use common::{TestDb, agent, commit, person, vendor};

#[tokio::test]
async fn a_person_edits_a_vendor_and_an_agent_only_changes_terms() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let (robert, ap_agent) = (person("robert"), agent("claude-ap"));
    let v = vendor(s, &ap_agent, "Kng Environmental").await;

    let r = commit(
        s,
        &robert,
        "vendor.update",
        json!({"vendor_id": v, "name": " King Environmental ", "email": "ar@king.example", "terms_days": 30}),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["name"], "King Environmental");
    assert_eq!(r["result"]["email"], "ar@king.example");
    assert_eq!(r["result"]["terms_days"], 30);

    // Left out keeps; null clears.
    let r = commit(
        s,
        &ap_agent,
        "vendor.update",
        json!({"vendor_id": v, "terms_days": null}),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["terms_days"], json!(null));
    assert_eq!(r["result"]["email"], "ar@king.example");

    // An agent can't move invoice matching or invitations to another address.
    for change in [
        json!({"name": "Someone Else"}),
        json!({"email": "pay@attacker.example"}),
        json!({"email": null}),
    ] {
        let mut input = json!({"vendor_id": v});
        input
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        let err = commit(s, &ap_agent, "vendor.update", input)
            .await
            .unwrap_err();
        assert!(matches!(err, CommandError::Forbidden(_)), "{err:?}");
    }
    // Sending the same name back is not a change, so an agent may.
    commit(
        s,
        &ap_agent,
        "vendor.update",
        json!({"vendor_id": v, "name": "King Environmental", "terms_days": 15}),
    )
    .await
    .unwrap();

    for bad in [
        json!({"name": "  "}),
        json!({"email": "not an email"}),
        json!({"terms_days": 400}),
    ] {
        let mut input = json!({"vendor_id": v});
        input
            .as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        let err = commit(s, &robert, "vendor.update", input)
            .await
            .unwrap_err();
        assert!(matches!(err, CommandError::Invalid(_)), "{err:?}");
    }

    let events = commit(s, &robert, "event.list", json!({"entity_id": v}))
        .await
        .unwrap();
    let updates: Vec<_> = events["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "vendor.updated")
        .collect();
    assert_eq!(updates.len(), 3);
    // Newest first: the first edit renamed it.
    assert_eq!(
        updates[2]["payload"]["changes"]["name"]["from"],
        "Kng Environmental"
    );
    assert_eq!(updates[2]["actor"], "robert");
    db.drop().await;
}

#[tokio::test]
async fn an_account_number_recorded_by_mistake_is_removed() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = &db.state;
    let ap_agent = agent("claude-ap");
    let v = vendor(s, &ap_agent, "Green Grounds").await;
    let other = vendor(s, &ap_agent, "Someone Else").await;
    let a = commit(
        s,
        &ap_agent,
        "vendor.add_account",
        json!({"vendor_id": v, "account_number": "4417"}),
    )
    .await
    .unwrap();
    let account_id = a["result"]["id"].as_str().unwrap().to_owned();

    // Only from the vendor it belongs to.
    let err = commit(
        s,
        &ap_agent,
        "vendor.remove_account",
        json!({"vendor_id": other, "account_id": account_id}),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CommandError::NotFound(_)), "{err:?}");

    commit(
        s,
        &ap_agent,
        "vendor.remove_account",
        json!({"vendor_id": v, "account_id": account_id}),
    )
    .await
    .unwrap();
    let got = commit(s, &ap_agent, "vendor.get", json!({"vendor_id": v}))
        .await
        .unwrap();
    assert_eq!(got["result"]["accounts"], json!([]));
    // And it can be added again.
    commit(
        s,
        &ap_agent,
        "vendor.add_account",
        json!({"vendor_id": v, "account_number": "4417"}),
    )
    .await
    .unwrap();
    db.drop().await;
}
