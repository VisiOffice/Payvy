//! Another system's purchase-order feed: upsert, standing authorization, and cancel
//! by external id.

use payvy_server::Actor;
use payvy_server::commands::CommandError;
use payvy_store::ActorKind;
use serde_json::{Value, json};
use uuid::Uuid;

mod common;
use common::{TestDb, agent, bill, commit, confirm, person, vendor};

struct Grant {
    auth_id: String,
    key_id: Uuid,
}

fn quarry(key_id: Uuid) -> Actor {
    agent("quarry").with_key(key_id)
}

async fn po_on(db: &TestDb) {
    commit(
        &db.state,
        &person("admin"),
        "po.configure",
        json!({ "enabled": true }),
    )
    .await
    .unwrap();
}

async fn add_robert(db: &TestDb) {
    let r = commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": "robert@example.com", "name": "Robert", "actor": "robert" }),
    )
    .await;
    if let Err(e) = r {
        assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    }
}

fn body(vendor_id: &str, number: &str, ext: &str, cents: i64) -> Value {
    json!({
        "vendor_id": vendor_id,
        "po_number": number,
        "external_source": "quarry",
        "external_id": ext,
        "authorized_cents": cents,
        "service_address": "1 Quarry Rd",
        "period_start": "2027-01-01",
        "period_end": "2027-01-28",
        "vendor_reference": "C-100",
        "final_period": false,
        "metadata": { "customer": "Allied", "order": "900" },
        "lines": [{ "description": "Base rent", "amount_cents": cents }],
    })
}

async fn authorize(db: &TestDb, vendor_id: &str, max_cents: i64) -> Grant {
    add_robert(db).await;
    let key_id = db
        .state
        .store
        .insert_api_key(
            payvy_store::DEFAULT_ORG,
            "quarry",
            ActorKind::Agent,
            &payvy_server::hash_key("quarry-feed-test"),
        )
        .await
        .unwrap();
    let granted = commit(
        &db.state,
        &person("robert"),
        "po.source_authorize",
        json!({
            "source": "quarry",
            "key_id": key_id,
            "vendor_ids": [vendor_id],
            "max_cents": max_cents,
            "owner": "robert",
        }),
    )
    .await
    .unwrap();
    assert_eq!(granted["result"]["key_id"], json!(key_id));
    Grant {
        auth_id: granted["result"]["id"].as_str().unwrap().to_owned(),
        key_id,
    }
}

async fn clerk(db: &TestDb) -> Actor {
    commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": "clerk@example.com", "name": "Clerk", "actor": "clerk", "roles": ["ap_clerk"] }),
    )
    .await
    .unwrap();
    Actor::person(payvy_store::DEFAULT_ORG, "clerk")
}

#[tokio::test]
async fn a_standing_authorization_activates_within_the_limit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Allied").await;

    let early = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap_err();
    assert!(early.to_string().contains("po_source"), "{early}");

    let e = commit(
        &db.state,
        &agent("quarry"),
        "po.source_authorize",
        json!({ "source": "quarry", "vendor_ids": [v], "max_cents": 100_000, "owner": "robert" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    let clerk = clerk(&db).await;
    let e = commit(
        &db.state,
        &clerk,
        "po.source_authorize",
        json!({ "source": "quarry", "vendor_ids": [v], "max_cents": 100_000, "owner": "robert" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Forbidden(_)), "{e}");

    let grant = authorize(&db, &v, 100_000).await;
    let q = quarry(grant.key_id);

    // Settings > Purchase orders lists the grant with its key and vendor names.
    let listed = commit(&db.state, &robert, "po.source_list", json!({}))
        .await
        .unwrap();
    let a = &listed["result"]["authorizations"][0];
    assert_eq!(a["id"], json!(grant.auth_id));
    assert_eq!(a["source"], "quarry");
    assert_eq!(a["key_actor"], "quarry");
    assert_eq!(a["key_revoked"], false);
    assert_eq!(a["vendors"][0]["name"], "Allied");
    assert_eq!(a["max_cents"], 100_000);
    assert!(a["revoked_at"].is_null());
    let active = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap();
    assert_eq!(active["result"]["outcome"], "activated");
    assert_eq!(active["result"]["status"], "active");
    assert_eq!(active["result"]["activated_by"], "robert");
    assert_eq!(active["result"]["owner"], "robert");
    assert_eq!(active["result"]["vendor_reference"], "C-100");

    let again = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap();
    assert_eq!(again["result"]["outcome"], "unchanged");
    assert_eq!(again["result"]["revision"]["revision"], 1);

    let mut named = body(&v, "900-P4", "period_4", 50_000);
    named["owner"] = json!("robert");
    let named_po = commit(&db.state, &q, "po.upsert", named).await.unwrap();
    assert_eq!(named_po["result"]["owner"], "robert");
    let kept = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P4", "period_4", 50_000),
    )
    .await
    .unwrap();
    assert_eq!(kept["result"]["outcome"], "unchanged");
    assert_eq!(kept["result"]["owner"], "robert");

    let over = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P2", "period_2", 150_000),
    )
    .await
    .unwrap();
    assert_eq!(over["result"]["outcome"], "created");
    assert_eq!(over["result"]["status"], "draft");
    let over_id = over["result"]["id"].as_str().unwrap().to_owned();

    let clerk_over = commit(
        &db.state,
        &clerk,
        "po.upsert",
        body(&v, "900-P2", "period_2", 90_000),
    )
    .await
    .unwrap_err();
    assert!(
        clerk_over.to_string().contains("po_needs_person"),
        "{clerk_over}"
    );
    let other_key = db
        .state
        .store
        .insert_api_key(
            payvy_store::DEFAULT_ORG,
            "quarry",
            ActorKind::Agent,
            &payvy_server::hash_key("quarry-draft-other"),
        )
        .await
        .unwrap();
    let other_over = commit(
        &db.state,
        &agent("quarry").with_key(other_key),
        "po.upsert",
        body(&v, "900-P2", "period_2", 80_000),
    )
    .await
    .unwrap_err();
    assert!(
        other_over.to_string().contains("po_needs_person"),
        "{other_over}"
    );
    let still_over = commit(&db.state, &robert, "po.get", json!({ "po_id": over_id }))
        .await
        .unwrap();
    assert_eq!(still_over["result"]["status"], "draft");
    assert_eq!(still_over["result"]["amounts"]["authorized_cents"], 150_000);
    let same_over = commit(
        &db.state,
        &clerk,
        "po.upsert",
        body(&v, "900-P2", "period_2", 150_000),
    )
    .await
    .unwrap();
    assert_eq!(same_over["result"]["outcome"], "unchanged");
    assert_eq!(same_over["result"]["status"], "draft");
    let still_high = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P2", "period_2", 120_000),
    )
    .await
    .unwrap();
    assert_eq!(still_high["result"]["outcome"], "updated");
    assert_eq!(still_high["result"]["status"], "draft");
    assert_eq!(still_high["result"]["amounts"]["authorized_cents"], 120_000);
    let brought_in = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P2", "period_2", 90_000),
    )
    .await
    .unwrap();
    assert_eq!(brought_in["result"]["outcome"], "activated");
    assert_eq!(brought_in["result"]["status"], "active");

    let stranger = commit(
        &db.state,
        &agent("inbox"),
        "po.upsert",
        body(&v, "900-P9", "period_9", 80_000),
    )
    .await
    .unwrap_err();
    assert!(stranger.to_string().contains("po_source"), "{stranger}");
    let clerk_draft = commit(
        &db.state,
        &clerk,
        "po.upsert",
        body(&v, "900-P8", "period_8", 80_000),
    )
    .await
    .unwrap_err();
    assert!(
        clerk_draft.to_string().contains("po_source"),
        "{clerk_draft}"
    );
    let squat = commit(
        &db.state,
        &robert,
        "po.import",
        json!({
            "vendor_id": v,
            "po_number": "900-S",
            "external_source": "quarry",
            "external_id": "squat",
            "authorized_cents": 10_000,
            "lines": [{ "description": "Base rent", "amount_cents": 10_000 }],
        }),
    )
    .await
    .unwrap_err();
    assert!(squat.to_string().contains("po_source"), "{squat}");
    let created = commit(
        &db.state,
        &clerk,
        "po.create",
        json!({
            "vendor_id": v,
            "po_number": "900-C",
            "external_source": "quarry",
            "external_id": "squat-create",
        }),
    )
    .await
    .unwrap_err();
    assert!(created.to_string().contains("po_source"), "{created}");

    let b = bill(&db.state, &agent("claude-ap"), &v, "final-1", 40_000).await;
    let mut final_body = body(&v, "900-P3", "period_3", 80_000);
    final_body["final_period"] = json!(true);
    let flagged = commit(&db.state, &q, "po.upsert", final_body)
        .await
        .unwrap();
    assert_eq!(flagged["result"]["final_period"], true);
    let preview = commit(
        &db.state,
        &agent("claude-ap"),
        "po.match_preview",
        json!({ "bill_id": b, "po_number": "C-100" }),
    )
    .await
    .unwrap();
    let candidates = preview["result"]["candidates"].as_array().unwrap();
    let hit = candidates
        .iter()
        .find(|c| c["po_number"] == "900-P3")
        .unwrap();
    assert_eq!(hit["final_match"], true);
    assert_eq!(hit["verified_reference"], true);

    let po_id = flagged["result"]["id"].as_str().unwrap();
    let used = bill(&db.state, &agent("claude-ap"), &v, "final-used", 50_000).await;
    confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({ "bill_id": used, "allocations": [{ "po_id": po_id, "amount_cents": 50_000 }] }),
    )
    .await
    .unwrap();
    let second = bill(&db.state, &agent("claude-ap"), &v, "final-2", 40_000).await;
    let again = commit(
        &db.state,
        &agent("claude-ap"),
        "po.match_preview",
        json!({ "bill_id": second, "po_number": "C-100" }),
    )
    .await
    .unwrap();
    let second_hit = again["result"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["po_number"] == "900-P3")
        .unwrap();
    assert_eq!(second_hit["available_cents"], 30_000);
    assert_eq!(second_hit["final_match"], false);
    let fits = bill(&db.state, &agent("claude-ap"), &v, "final-3", 20_000).await;
    let third = commit(
        &db.state,
        &agent("claude-ap"),
        "po.match_preview",
        json!({ "bill_id": fits, "po_number": "C-100" }),
    )
    .await
    .unwrap();
    let third_hit = third["result"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["po_number"] == "900-P3")
        .unwrap();
    assert_eq!(third_hit["final_match"], true);
    db.drop().await;
}

#[tokio::test]
async fn active_changes_keep_allocations_and_stop_at_the_limit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Allied").await;
    let grant = authorize(&db, &v, 100_000).await;
    let q = quarry(grant.key_id);
    let created = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 100_000),
    )
    .await
    .unwrap();
    let id = created["result"]["id"].as_str().unwrap();

    let clerk = clerk(&db).await;
    let clerk_edit = commit(
        &db.state,
        &clerk,
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap_err();
    assert!(
        clerk_edit.to_string().contains("po_needs_person"),
        "{clerk_edit}"
    );
    let mut inbox_body = body(&v, "900-P1", "period_1", 100_000);
    inbox_body["period_end"] = json!("2027-03-01");
    let inbox_edit = commit(&db.state, &agent("inbox"), "po.upsert", inbox_body)
        .await
        .unwrap_err();
    assert!(
        inbox_edit.to_string().contains("po_needs_person"),
        "{inbox_edit}"
    );
    let twin = db
        .state
        .store
        .insert_api_key(
            payvy_store::DEFAULT_ORG,
            "quarry",
            ActorKind::Agent,
            &payvy_server::hash_key("quarry-other-key"),
        )
        .await
        .unwrap();
    let twin_edit = commit(
        &db.state,
        &agent("quarry").with_key(twin),
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap_err();
    assert!(
        twin_edit.to_string().contains("po_needs_person"),
        "{twin_edit}"
    );
    let untouched = commit(&db.state, &robert, "po.get", json!({ "po_id": id }))
        .await
        .unwrap();
    assert_eq!(untouched["result"]["amounts"]["authorized_cents"], 100_000);
    assert_eq!(untouched["result"]["period_end"], "2027-01-28");

    let b = bill(&db.state, &agent("claude-ap"), &v, "inv-1", 60_000).await;
    confirm(
        &db.state,
        &agent("claude-ap"),
        "po.match_apply",
        json!({ "bill_id": b, "allocations": [{ "po_id": id, "amount_cents": 60_000 }] }),
    )
    .await
    .unwrap();

    let down = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 70_000),
    )
    .await
    .unwrap();
    assert_eq!(down["result"]["outcome"], "revised");
    assert_eq!(down["result"]["amounts"]["authorized_cents"], 70_000);
    assert_eq!(down["result"]["amounts"]["reserved_cents"], 60_000);
    assert_eq!(down["result"]["revision"]["revision"], 2);

    let too_low = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 50_000),
    )
    .await
    .unwrap_err();
    assert!(too_low.to_string().contains("po_needs_person"), "{too_low}");
    let still = commit(&db.state, &robert, "po.get", json!({ "po_id": id }))
        .await
        .unwrap();
    assert_eq!(still["result"]["amounts"]["authorized_cents"], 70_000);
    assert_eq!(still["result"]["amounts"]["reserved_cents"], 60_000);

    let up = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 90_000),
    )
    .await
    .unwrap();
    assert_eq!(up["result"]["outcome"], "revised");
    assert_eq!(up["result"]["amounts"]["reserved_cents"], 60_000);

    let over = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 120_000),
    )
    .await
    .unwrap_err();
    assert!(over.to_string().contains("po_needs_person"), "{over}");

    let mut moved = body(&v, "900-P1", "period_1", 90_000);
    moved["period_end"] = json!("2027-02-04");
    moved["service_address"] = json!("2 Quarry Rd");
    let revised = commit(&db.state, &q, "po.upsert", moved).await.unwrap();
    assert_eq!(revised["result"]["outcome"], "revised");
    assert_eq!(revised["result"]["period_end"], "2027-02-04");
    assert_eq!(revised["result"]["service_address"], "2 Quarry Rd");
    assert_eq!(revised["result"]["owner"], "robert");
    assert_eq!(revised["result"]["amounts"]["reserved_cents"], 60_000);

    let other = vendor(&db.state, &robert, "Other").await;
    let mut switched = body(&other, "900-P1", "period_1", 90_000);
    switched["period_end"] = json!("2027-02-04");
    switched["service_address"] = json!("2 Quarry Rd");
    let e = commit(&db.state, &q, "po.upsert", switched)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("po_identity"), "{e}");

    let e = commit(
        &db.state,
        &q,
        "po.cancel",
        json!({ "po_id": id, "reason": "voided in quarry" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let e = commit(
        &db.state,
        &q,
        "po.cancel",
        json!({ "external_source": "quarry", "external_id": "period_1", "reason": "voided in quarry" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("allocations_reserved"), "{e}");
    assert_eq!(
        commit(&db.state, &robert, "po.get", json!({ "po_id": id }))
            .await
            .unwrap()["result"]["status"],
        "active"
    );
    db.drop().await;
}

#[tokio::test]
async fn revoke_stops_later_activation_and_a_repeat_cancel_returns() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Allied").await;
    let other = vendor(&db.state, &robert, "Other").await;
    let foreign = commit(
        &db.state,
        &robert,
        "po.import",
        json!({
            "vendor_id": other,
            "po_number": "900-X",
            "external_source": "quarry",
            "external_id": "other_1",
            "authorized_cents": 40_000,
            "lines": [{ "description": "Base rent", "amount_cents": 40_000 }],
        }),
    )
    .await
    .unwrap();
    assert_eq!(foreign["result"]["status"], "draft");
    let foreign_id = foreign["result"]["id"].as_str().unwrap().to_owned();

    let grant = authorize(&db, &v, 100_000).await;
    let q = quarry(grant.key_id);
    let e = commit(
        &db.state,
        &q,
        "po.cancel",
        json!({ "external_source": "quarry", "external_id": "other_1", "reason": "wrong vendor" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("po_source"), "{e}");
    assert_eq!(
        commit(&db.state, &robert, "po.get", json!({ "po_id": foreign_id }))
            .await
            .unwrap()["result"]["status"],
        "draft"
    );
    let e = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&other, "900-O", "other_2", 40_000),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("po_source"), "{e}");

    let first = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 40_000),
    )
    .await
    .unwrap();
    assert_eq!(first["result"]["status"], "active");

    let open = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P2", "period_2", 40_000),
    )
    .await
    .unwrap();
    assert_eq!(open["result"]["status"], "active");
    let canceled = commit(
        &db.state,
        &q,
        "po.cancel",
        json!({ "external_source": "quarry", "external_id": "period_2", "reason": "order voided" }),
    )
    .await
    .unwrap();
    assert_eq!(canceled["result"]["status"], "canceled");
    let repeat = commit(
        &db.state,
        &q,
        "po.cancel",
        json!({ "external_source": "quarry", "external_id": "period_2", "reason": "order voided" }),
    )
    .await
    .unwrap();
    assert_eq!(repeat["result"]["outcome"], "unchanged");
    assert_eq!(repeat["result"]["status"], "canceled");
    let closed = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P2", "period_2", 40_000),
    )
    .await
    .unwrap_err();
    assert!(closed.to_string().contains("po_closed"), "{closed}");

    let e = commit(
        &db.state,
        &agent("inbox"),
        "po.cancel",
        json!({ "external_source": "quarry", "external_id": "period_1", "reason": "not our feed" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("po_source"), "{e}");
    let imported = commit(
        &db.state,
        &robert,
        "po.import",
        json!({
            "vendor_id": v,
            "po_number": "900-L",
            "external_source": "legacy",
            "external_id": "legacy_1",
            "authorized_cents": 40_000,
            "lines": [{ "description": "Base rent", "amount_cents": 40_000 }],
        }),
    )
    .await
    .unwrap();
    assert_eq!(imported["result"]["status"], "draft");
    let legacy_id = imported["result"]["id"].as_str().unwrap().to_owned();
    let e = commit(
        &db.state,
        &q,
        "po.cancel",
        json!({ "external_source": "legacy", "external_id": "legacy_1", "reason": "wrong source" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("po_source"), "{e}");
    assert_eq!(
        commit(&db.state, &robert, "po.get", json!({ "po_id": legacy_id }))
            .await
            .unwrap()["result"]["status"],
        "draft"
    );

    let revoked = commit(
        &db.state,
        &robert,
        "po.source_revoke",
        json!({ "authorization_id": grant.auth_id }),
    )
    .await
    .unwrap();
    assert_eq!(revoked["result"]["outcome"], "revoked");
    let again = commit(
        &db.state,
        &robert,
        "po.source_revoke",
        json!({ "authorization_id": grant.auth_id }),
    )
    .await
    .unwrap();
    assert_eq!(again["result"]["outcome"], "unchanged");

    let later = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P3", "period_3", 40_000),
    )
    .await
    .unwrap_err();
    assert!(later.to_string().contains("po_source"), "{later}");
    assert_eq!(
        commit(
            &db.state,
            &robert,
            "po.get",
            json!({ "po_id": first["result"]["id"] })
        )
        .await
        .unwrap()["result"]["status"],
        "active"
    );

    let e = commit(
        &db.state,
        &q,
        "po.cancel",
        json!({ "external_source": "quarry", "external_id": "period_1", "reason": "revoked" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("po_source"), "{e}");
    assert_eq!(
        commit(
            &db.state,
            &robert,
            "po.get",
            json!({ "po_id": first["result"]["id"] })
        )
        .await
        .unwrap()["result"]["status"],
        "active"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_person_edit_is_not_undone_by_the_sync() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Allied").await;
    let grant = authorize(&db, &v, 100_000).await;
    let q = quarry(grant.key_id);
    let created = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap();
    assert_eq!(created["result"]["status"], "active");
    let id = created["result"]["id"].as_str().unwrap();
    let open = commit(
        &db.state,
        &robert,
        "po.release_hold",
        json!({ "po_id": id }),
    )
    .await
    .unwrap();
    assert_eq!(open["result"]["outcome"], "unchanged");
    commit(
        &db.state,
        &robert,
        "po.update",
        json!({
            "po_id": id,
            "authorized_cents": 60_000,
            "reason": "cut to the agreed amount",
            "lines": [{ "description": "Base rent", "amount_cents": 60_000 }],
        }),
    )
    .await
    .unwrap();

    let undone = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap_err();
    assert!(undone.to_string().contains("po_needs_person"), "{undone}");
    let held = commit(&db.state, &robert, "po.get", json!({ "po_id": id }))
        .await
        .unwrap();
    assert_eq!(held["result"]["amounts"]["authorized_cents"], 60_000);
    assert_eq!(held["result"]["period_end"], "2027-01-28");

    let caught_up = commit(
        &db.state,
        &q,
        "po.upsert",
        body(&v, "900-P1", "period_1", 60_000),
    )
    .await
    .unwrap();
    assert_eq!(caught_up["result"]["outcome"], "unchanged");
    assert_eq!(caught_up["result"]["amounts"]["authorized_cents"], 60_000);

    let mut moved = body(&v, "900-P1", "period_1", 60_000);
    moved["period_end"] = json!("2027-02-04");
    let still_held = commit(&db.state, &q, "po.upsert", moved).await.unwrap_err();
    assert!(
        still_held.to_string().contains("po_needs_person"),
        "{still_held}"
    );
    assert_eq!(
        commit(&db.state, &robert, "po.get", json!({ "po_id": id }))
            .await
            .unwrap()["result"]["period_end"],
        "2027-01-28"
    );

    let e = commit(&db.state, &q, "po.release_hold", json!({ "po_id": id }))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let cleared = commit(
        &db.state,
        &robert,
        "po.release_hold",
        json!({ "po_id": id }),
    )
    .await
    .unwrap();
    assert_eq!(cleared["result"]["outcome"], "cleared");
    let again = commit(
        &db.state,
        &robert,
        "po.release_hold",
        json!({ "po_id": id }),
    )
    .await
    .unwrap();
    assert_eq!(again["result"]["outcome"], "unchanged");
    let mut released = body(&v, "900-P1", "period_1", 60_000);
    released["period_end"] = json!("2027-02-04");
    let revised = commit(&db.state, &q, "po.upsert", released).await.unwrap();
    assert_eq!(revised["result"]["outcome"], "revised");
    assert_eq!(revised["result"]["period_end"], "2027-02-04");
    assert_eq!(revised["result"]["owner"], "robert");
    db.drop().await;
}

#[tokio::test]
async fn two_upserts_racing_create_one_po_and_a_vendor_links_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Allied").await;
    let other = vendor(&db.state, &robert, "Other").await;
    let grant = authorize(&db, &v, 100_000).await;
    let q = quarry(grant.key_id);
    let run = {
        let state = &db.state;
        let v = v.clone();
        let q = q.clone();
        move || {
            let state = state;
            let v = v.clone();
            let q = q.clone();
            async move {
                commit(
                    state,
                    &q,
                    "po.upsert",
                    body(&v, "900-P1", "period_1", 10_000),
                )
                .await
            }
        }
    };
    let (a, b) = tokio::join!(run(), run());
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a["result"]["id"], b["result"]["id"]);
    let listed = commit(&db.state, &robert, "po.list", json!({}))
        .await
        .unwrap();
    assert_eq!(
        listed["result"]["purchase_orders"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let linked = commit(
        &db.state,
        &robert,
        "vendor.link_external",
        json!({ "vendor_id": v, "source": "quarry", "external_id": "9" }),
    )
    .await
    .unwrap();
    assert_eq!(linked["result"]["outcome"], "linked");
    let same = commit(
        &db.state,
        &robert,
        "vendor.link_external",
        json!({ "vendor_id": v, "source": "quarry", "external_id": "9" }),
    )
    .await
    .unwrap();
    assert_eq!(same["result"]["outcome"], "unchanged");
    let e = commit(
        &db.state,
        &robert,
        "vendor.link_external",
        json!({ "vendor_id": other, "source": "quarry", "external_id": "9" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    let e = commit(
        &db.state,
        &agent("quarry"),
        "vendor.link_external",
        json!({ "vendor_id": v, "source": "quarry", "external_id": "10" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    db.drop().await;
}

async fn bill_at(db: &TestDb, v: &str, number: &str, cents: i64, extra: Value) -> Value {
    let mut input = json!({ "vendor_id": v, "invoice_number": number,
                            "invoice_date": "2027-01-15", "amount_cents": cents });
    for (k, val) in extra.as_object().unwrap() {
        input[k] = val.clone();
    }
    commit(&db.state, &agent("email"), "bill.create", input)
        .await
        .unwrap()["result"]
        .clone()
}

async fn allocations(db: &TestDb, bill_id: &str) -> Vec<Value> {
    let got = commit(
        &db.state,
        &person("robert"),
        "bill.get",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap();
    got["result"]["purchase_orders"]["allocations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_bill_goes_against_the_one_po_it_plainly_belongs_to_whichever_arrives_first() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Local Bin").await;
    let grant = authorize(&db, &v, 200_000).await;
    let feed = quarry(grant.key_id);

    // Bill first: an invoice for 8932 Beverlywood arrives before its PO.
    let early = bill_at(
        &db,
        &v,
        "BEV-1",
        75_000,
        json!({ "service_address": "8932 BEVERLYWOOD ST." }),
    )
    .await;
    let early_id = early["bill"]["id"].as_str().unwrap().to_owned();
    assert!(early.get("auto_matched").is_none(), "no PO yet: {early}");

    // The PO arrives: active within the grant, and the waiting bill is
    // reserved against it, recorded as Payvy's workflow agent.
    let mut po = body(&v, "Q-BEV", "order-2495", 75_000);
    po["service_address"] = json!("8932 Beverlywood Street, Los Angeles, CA, 90034");
    po["vendor_reference"] = Value::Null;
    let sent = commit(&db.state, &feed, "po.upsert", po.clone())
        .await
        .unwrap();
    assert_eq!(sent["result"]["status"], "active");
    let placed = sent["result"]["auto_matched"].as_array().unwrap();
    assert_eq!(placed.len(), 1, "{sent}");
    assert_eq!(placed[0]["bill_id"], json!(early_id));
    assert_eq!(placed[0]["rule"], "service_address");
    let a = allocations(&db, &early_id).await;
    assert_eq!(a.len(), 1, "{a:?}");
    assert_eq!(a[0]["state"], "reserved");
    assert_eq!(a[0]["amount_cents"], 75_000);
    let mut tx = db
        .state
        .store
        .begin(payvy_store::DEFAULT_ORG)
        .await
        .unwrap();
    let events = payvy_store::list_events(&mut tx, None, 200).await.unwrap();
    tx.rollback().await.unwrap();
    let ev = serde_json::to_value(&events).unwrap();
    let applied: Vec<&Value> = ev
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "po.match_applied")
        .collect();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0]["actor"], "workflow");
    assert_eq!(applied[0]["payload"]["automatic"], true);
    assert_eq!(applied[0]["payload"]["triggered_by"], "quarry");

    // Sending the same PO again places nothing more.
    let again = commit(&db.state, &feed, "po.upsert", po).await.unwrap();
    assert!(again["result"].get("auto_matched").is_none(), "{again}");

    // PO first: a PO at 1500 Harwood, then its invoice.
    let mut harwood = body(&v, "Q-HAR", "order-2600", 40_000);
    harwood["service_address"] = json!("1500 Harwood St, Los Angeles");
    harwood["vendor_reference"] = Value::Null;
    commit(&db.state, &feed, "po.upsert", harwood)
        .await
        .unwrap();
    let on_time = bill_at(
        &db,
        &v,
        "HAR-1",
        40_000,
        json!({ "service_address": "1500 Harwood Street" }),
    )
    .await;
    assert_eq!(on_time["auto_matched"]["po_number"], "Q-HAR", "{on_time}");

    // More than the PO has left is a person's choice.
    let over = bill_at(
        &db,
        &v,
        "HAR-2",
        1_000,
        json!({ "service_address": "1500 Harwood Street" }),
    )
    .await;
    assert!(over.get("auto_matched").is_none(), "{over}");

    // A printed PO number decides, and one that names no PO matches
    // nothing, even at a known address.
    let mut numbered = body(&v, "Q-ELM", "order-2700", 30_000);
    numbered["service_address"] = json!("9 Elm St");
    numbered["vendor_reference"] = Value::Null;
    commit(&db.state, &feed, "po.upsert", numbered)
        .await
        .unwrap();
    let unknown = bill_at(
        &db,
        &v,
        "ELM-1",
        10_000,
        json!({ "po_number": "Q-NOPE", "service_address": "9 Elm St" }),
    )
    .await;
    assert!(unknown.get("auto_matched").is_none(), "{unknown}");
    let named = bill_at(&db, &v, "ELM-2", 10_000, json!({ "po_number": "q elm" })).await;
    assert_eq!(named["auto_matched"]["rule"], "po_number", "{named}");

    // Two open POs at one address: nobody guesses.
    let mut twin = body(&v, "Q-ELM-2", "order-2701", 30_000);
    twin["service_address"] = json!("9 Elm Street");
    twin["vendor_reference"] = Value::Null;
    commit(&db.state, &feed, "po.upsert", twin).await.unwrap();
    let twice = bill_at(
        &db,
        &v,
        "ELM-3",
        5_000,
        json!({ "service_address": "9 Elm St" }),
    )
    .await;
    assert!(twice.get("auto_matched").is_none(), "{twice}");

    // A bill with nothing printed to go on is never matched.
    let bare = bill_at(&db, &v, "BARE-1", 5_000, json!({})).await;
    assert!(bare.get("auto_matched").is_none(), "{bare}");
    db.drop().await;
}
