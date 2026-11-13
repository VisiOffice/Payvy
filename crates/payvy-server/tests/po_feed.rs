//! Quarry's purchase-order feed: upsert, standing authorization, and cancel
//! by external id.

use payvy_server::Actor;
use payvy_server::commands::CommandError;
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, bill, commit, confirm, person, vendor};

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

async fn authorize(db: &TestDb, vendor_id: &str, max_cents: i64) -> String {
    add_robert(db).await;
    db.state
        .store
        .insert_api_key(
            payvy_store::DEFAULT_ORG,
            "quarry",
            payvy_store::ActorKind::Agent,
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
            "agent": "quarry",
            "vendor_ids": [vendor_id],
            "max_cents": max_cents,
            "owner": "robert",
        }),
    )
    .await
    .unwrap();
    assert_eq!(granted["result"]["agent"], "quarry");
    granted["result"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_standing_authorization_activates_within_the_limit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    po_on(&db).await;
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Allied").await;

    let draft = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap();
    assert_eq!(draft["result"]["outcome"], "created");
    assert_eq!(draft["result"]["status"], "draft");
    assert_eq!(draft["result"]["activated_by"], Value::Null);
    let id = draft["result"]["id"].as_str().unwrap().to_owned();

    let e = commit(
        &db.state,
        &agent("quarry"),
        "po.source_authorize",
        json!({ "source": "quarry", "vendor_ids": [v], "max_cents": 100_000, "owner": "robert" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    let clerk = {
        commit(
            &db.state,
            &person("admin"),
            "person.add",
            json!({ "email": "clerk@example.com", "name": "Clerk", "actor": "clerk", "roles": ["ap_clerk"] }),
        )
        .await
        .unwrap();
        Actor::person(payvy_store::DEFAULT_ORG, "clerk")
    };
    let e = commit(
        &db.state,
        &clerk,
        "po.source_authorize",
        json!({ "source": "quarry", "vendor_ids": [v], "max_cents": 100_000, "owner": "robert" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Forbidden(_)), "{e}");

    authorize(&db, &v, 100_000).await;
    let active = commit(
        &db.state,
        &agent("quarry"),
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
    assert_eq!(active["result"]["id"], id);

    let again = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap();
    assert_eq!(again["result"]["outcome"], "unchanged");
    assert_eq!(again["result"]["revision"]["revision"], 1);

    let over = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P2", "period_2", 150_000),
    )
    .await
    .unwrap();
    assert_eq!(over["result"]["outcome"], "created");
    assert_eq!(over["result"]["status"], "draft");

    let stranger = commit(
        &db.state,
        &agent("inbox"),
        "po.upsert",
        body(&v, "900-P9", "period_9", 80_000),
    )
    .await
    .unwrap();
    assert_eq!(stranger["result"]["outcome"], "created");
    assert_eq!(stranger["result"]["status"], "draft");
    assert_eq!(stranger["result"]["activated_by"], Value::Null);
    let clerk_draft = commit(
        &db.state,
        &clerk,
        "po.upsert",
        body(&v, "900-P8", "period_8", 80_000),
    )
    .await
    .unwrap();
    assert_eq!(clerk_draft["result"]["status"], "draft");

    let b = bill(&db.state, &agent("claude-ap"), &v, "final-1", 40_000).await;
    let mut final_body = body(&v, "900-P3", "period_3", 80_000);
    final_body["final_period"] = json!(true);
    let flagged = commit(&db.state, &agent("quarry"), "po.upsert", final_body)
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
    authorize(&db, &v, 100_000).await;
    let created = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P1", "period_1", 100_000),
    )
    .await
    .unwrap();
    let id = created["result"]["id"].as_str().unwrap();
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
        &agent("quarry"),
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
        &agent("quarry"),
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
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P1", "period_1", 90_000),
    )
    .await
    .unwrap();
    assert_eq!(up["result"]["outcome"], "revised");
    assert_eq!(up["result"]["amounts"]["reserved_cents"], 60_000);

    let over = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P1", "period_1", 120_000),
    )
    .await
    .unwrap_err();
    assert!(over.to_string().contains("po_needs_person"), "{over}");

    let mut moved = body(&v, "900-P1", "period_1", 90_000);
    moved["period_end"] = json!("2027-02-04");
    moved["service_address"] = json!("2 Quarry Rd");
    let revised = commit(&db.state, &agent("quarry"), "po.upsert", moved)
        .await
        .unwrap();
    assert_eq!(revised["result"]["outcome"], "revised");
    assert_eq!(revised["result"]["period_end"], "2027-02-04");
    assert_eq!(revised["result"]["service_address"], "2 Quarry Rd");
    assert_eq!(revised["result"]["amounts"]["reserved_cents"], 60_000);

    let other = vendor(&db.state, &robert, "Other").await;
    let mut switched = body(&other, "900-P1", "period_1", 90_000);
    switched["period_end"] = json!("2027-02-04");
    switched["service_address"] = json!("2 Quarry Rd");
    let e = commit(&db.state, &agent("quarry"), "po.upsert", switched)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("po_identity"), "{e}");

    let e = commit(
        &db.state,
        &agent("quarry"),
        "po.cancel",
        json!({ "po_id": id, "reason": "voided in quarry" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let e = commit(
        &db.state,
        &agent("quarry"),
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
    let auth = authorize(&db, &v, 100_000).await;
    let first = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P1", "period_1", 40_000),
    )
    .await
    .unwrap();
    assert_eq!(first["result"]["status"], "active");

    let open = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P2", "period_2", 40_000),
    )
    .await
    .unwrap();
    assert_eq!(open["result"]["status"], "active");
    let canceled = commit(
        &db.state,
        &agent("quarry"),
        "po.cancel",
        json!({ "external_source": "quarry", "external_id": "period_2", "reason": "order voided" }),
    )
    .await
    .unwrap();
    assert_eq!(canceled["result"]["status"], "canceled");
    let repeat = commit(
        &db.state,
        &agent("quarry"),
        "po.cancel",
        json!({ "external_source": "quarry", "external_id": "period_2", "reason": "order voided" }),
    )
    .await
    .unwrap();
    assert_eq!(repeat["result"]["outcome"], "unchanged");
    assert_eq!(repeat["result"]["status"], "canceled");
    let closed = commit(
        &db.state,
        &agent("quarry"),
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
    let mut legacy = body(&v, "900-L", "legacy_1", 40_000);
    legacy["external_source"] = json!("legacy");
    let imported = commit(&db.state, &agent("quarry"), "po.upsert", legacy)
        .await
        .unwrap();
    assert_eq!(imported["result"]["status"], "draft");
    let e = commit(
        &db.state,
        &agent("quarry"),
        "po.cancel",
        json!({ "external_source": "legacy", "external_id": "legacy_1", "reason": "wrong source" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("po_source"), "{e}");

    let revoked = commit(
        &db.state,
        &robert,
        "po.source_revoke",
        json!({ "authorization_id": auth }),
    )
    .await
    .unwrap();
    assert_eq!(revoked["result"]["outcome"], "revoked");
    let again = commit(
        &db.state,
        &robert,
        "po.source_revoke",
        json!({ "authorization_id": auth }),
    )
    .await
    .unwrap();
    assert_eq!(again["result"]["outcome"], "unchanged");

    let later = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P3", "period_3", 40_000),
    )
    .await
    .unwrap();
    assert_eq!(later["result"]["status"], "draft");
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
        &agent("quarry"),
        "po.cancel",
        json!({ "external_source": "quarry", "external_id": "period_3", "reason": "revoked" }),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("po_source"), "{e}");
    assert_eq!(
        commit(
            &db.state,
            &robert,
            "po.get",
            json!({ "po_id": later["result"]["id"] })
        )
        .await
        .unwrap()["result"]["status"],
        "draft"
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
    authorize(&db, &v, 100_000).await;
    let created = commit(
        &db.state,
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P1", "period_1", 80_000),
    )
    .await
    .unwrap();
    assert_eq!(created["result"]["status"], "active");
    let id = created["result"]["id"].as_str().unwrap();
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
        &agent("quarry"),
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
        &agent("quarry"),
        "po.upsert",
        body(&v, "900-P1", "period_1", 60_000),
    )
    .await
    .unwrap();
    assert_eq!(caught_up["result"]["outcome"], "unchanged");
    assert_eq!(caught_up["result"]["amounts"]["authorized_cents"], 60_000);

    let mut moved = body(&v, "900-P1", "period_1", 60_000);
    moved["period_end"] = json!("2027-02-04");
    let still_held = commit(&db.state, &agent("quarry"), "po.upsert", moved)
        .await
        .unwrap_err();
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
    let run = {
        let state = &db.state;
        let v = v.clone();
        move || {
            let state = state;
            let v = v.clone();
            async move {
                commit(
                    state,
                    &agent("quarry"),
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
