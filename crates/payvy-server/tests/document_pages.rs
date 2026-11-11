//! Arranging a document's pages, against a real database (with row-level
//! security) and a real S3-compatible store: reorder, rotate and hide as
//! versions on top of the unchanged original, the bill's pin, and the lock
//! after approval.

use axum::Json;
use axum::routing::post;
use base64::Engine;
use payvy_extract::{Config, Extractor, Provider};
use payvy_server::Actor;
use payvy_server::commands::CommandError;
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, commit, person, set_workflow, vendor};

/// A PDF with `n` blank pages, different per call so uploads don't collide.
fn pdf(n: usize) -> Vec<u8> {
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", i + 3)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
    ];
    for _ in 0..n {
        objects.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into());
    }
    let mut out = format!("%PDF-1.4\n% {}\n", uuid::Uuid::new_v4());
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out += &format!("{} 0 obj\n{o}\nendobj\n", i + 1);
    }
    let xref = out.len();
    out += &format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for o in offsets {
        out += &format!("{o:010} 00000 n \n");
    }
    out += &format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.into_bytes()
}

async fn upload(db: &TestDb, who: &Actor, bytes: &[u8]) -> String {
    let up = commit(
        &db.state,
        who,
        "document.upload",
        json!({ "filename": "statement.pdf",
                "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes) }),
    )
    .await
    .unwrap();
    up["result"]["id"].as_str().unwrap().to_owned()
}

async fn get(db: &TestDb, who: &Actor, id: &str) -> Value {
    commit(&db.state, who, "document.get", json!({ "document_id": id }))
        .await
        .unwrap()["result"]
        .clone()
}

async fn arrange(
    db: &TestDb,
    who: &Actor,
    id: &str,
    expected_version: i64,
    pages: Value,
) -> Result<Value, CommandError> {
    commit(
        &db.state,
        who,
        "document.arrange",
        json!({ "document_id": id, "expected_version": expected_version, "pages": pages }),
    )
    .await
}

fn order(pages: &[u32]) -> Value {
    json!(
        pages
            .iter()
            .map(|p| json!({ "page": p }))
            .collect::<Vec<_>>()
    )
}

#[tokio::test]
async fn pages_are_arranged_in_versions_over_the_unchanged_original() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let file = pdf(4);
    let id = upload(&db, &robert, &file).await;

    // A fresh upload: four pages in order, version 0, nothing locked.
    let d = get(&db, &robert, &id).await;
    assert_eq!(d["document"]["page_count"], 4);
    assert_eq!(d["pages"]["count"], 4);
    assert_eq!(d["pages"]["version"], 0);
    assert_eq!(d["pages"]["arrangement_id"], Value::Null);
    assert_eq!(
        d["pages"]["arrangement"],
        json!([
            { "page": 1, "rotation": 0, "hidden": false },
            { "page": 2, "rotation": 0, "hidden": false },
            { "page": 3, "rotation": 0, "hidden": false },
            { "page": 4, "rotation": 0, "hidden": false },
        ])
    );
    assert_eq!(d["pages"]["locked"], false);
    assert_eq!(d["pages"]["versions"], json!([]));

    // Anything but an exact permutation is refused, as are odd rotations
    // and hiding every page.
    for (pages, says) in [
        (order(&[1, 2, 3]), "page 4 is missing"),
        (order(&[1, 2, 3, 3]), "page 3 is listed more than once"),
        (order(&[1, 2, 3, 4, 5]), "page 5 doesn't exist"),
        (order(&[0, 1, 2, 3]), "page 0 doesn't exist"),
        (
            json!([{ "page": 1, "rotation": 45 }, { "page": 2 }, { "page": 3 }, { "page": 4 }]),
            "rotation is 0, 90, 180 or 270",
        ),
        (
            json!([1, 2, 3, 4].map(|p| json!({ "page": p, "hidden": true }))),
            "at least one page must stay visible",
        ),
    ] {
        let e = arrange(&db, &robert, &id, 0, pages).await.unwrap_err();
        assert!(
            matches!(&e, CommandError::Invalid(m) if m.contains(says)),
            "{says}: {e}"
        );
    }
    let e = arrange(
        &db,
        &robert,
        &id,
        0,
        json!([{ "page": 1, "deleted": true }]),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    // The same arrangement again changes nothing, which is an error.
    let e = arrange(&db, &robert, &id, 0, order(&[1, 2, 3, 4]))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("already arranged")),
        "{e}"
    );

    // Page 3 to the front: version 1, with the change in words.
    let r = arrange(&db, &robert, &id, 0, order(&[3, 1, 2, 4]))
        .await
        .unwrap();
    assert_eq!(r["events"], json!(["document.arranged"]));
    assert_eq!(r["result"]["pages"]["version"], 1);
    assert_eq!(r["result"]["pages"]["arrangement"][0]["page"], 3);

    // Rotate page 2 and hide page 4 in one change.
    let v2 = json!([
        { "page": 3 }, { "page": 1 }, { "page": 2, "rotation": 90 }, { "page": 4, "hidden": true }
    ]);
    arrange(&db, &robert, &id, 1, v2.clone()).await.unwrap();

    // Someone working from version 1 is told it moved on.
    let e = arrange(&db, &agent("claude-ap"), &id, 1, order(&[1, 2, 3, 4]))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("version 2")),
        "{e}"
    );

    // Restore page 4.
    let v3 = json!([
        { "page": 3 }, { "page": 1 }, { "page": 2, "rotation": 90 }, { "page": 4 }
    ]);
    arrange(&db, &robert, &id, 2, v3).await.unwrap();

    let d = get(&db, &robert, &id).await;
    assert_eq!(d["pages"]["version"], 3);
    let versions = d["pages"]["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 3, "every version is kept");
    assert_eq!(versions[1]["version"], 2);
    assert_eq!(versions[1]["created_by"], "robert");
    assert_eq!(versions[1]["pages"][3]["hidden"], true);
    let said: Vec<&str> = d["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "document.arranged")
        .map(|e| e["payload"]["summary"].as_str().unwrap())
        .collect();
    assert_eq!(
        said,
        [
            "restored page 4",
            "rotated page 2 to 90° and hid page 4",
            "moved page 3 to first",
        ]
    );
    let first = d["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "document.arranged" && e["payload"]["version"] == 1)
        .unwrap();
    assert_eq!(
        first["payload"]["changes"],
        json!([{ "change": "moved", "page": 3, "to": 1, "of": 4 }])
    );

    // The original is untouched.
    let link = d["download_url"].as_str().unwrap();
    let got = reqwest::get(link).await.unwrap().bytes().await.unwrap();
    assert_eq!(got.as_ref(), file.as_slice());
    db.drop().await;
}

#[tokio::test]
async fn an_agent_arranges_under_its_own_name_and_images_are_one_page() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let ap = agent("claude-ap");
    let id = upload(&db, &person("robert"), &pdf(2)).await;
    let r = arrange(
        &db,
        &ap,
        &id,
        0,
        json!([{ "page": 1 }, { "page": 2, "hidden": true }]),
    )
    .await
    .unwrap();
    assert_eq!(
        r["result"]["pages"]["versions"][0]["created_by"],
        "claude-ap"
    );
    let events = commit(&db.state, &ap, "event.list", json!({ "entity_id": id }))
        .await
        .unwrap();
    let arranged = &events["result"]["events"][0];
    assert_eq!(arranged["kind"], "document.arranged");
    assert_eq!(arranged["actor"], "claude-ap");
    assert_eq!(arranged["payload"]["summary"], "hid page 2");

    // A photo is one page: it can be turned, not reordered or emptied.
    let jpeg = [
        &[0xFF, 0xD8, 0xFF, 0xE0][..],
        uuid::Uuid::new_v4().as_bytes(),
    ]
    .concat();
    let photo = upload(&db, &ap, &jpeg).await;
    assert_eq!(get(&db, &ap, &photo).await["pages"]["count"], 1);
    let r = arrange(&db, &ap, &photo, 0, json!([{ "page": 1, "rotation": 180 }]))
        .await
        .unwrap();
    assert_eq!(r["result"]["pages"]["arrangement"][0]["rotation"], 180);

    // A file that isn't really a PDF can't be counted or arranged, and
    // nothing crashes.
    let broken = format!("%PDF-1.4\n% {}\n%%EOF\n", uuid::Uuid::new_v4());
    let broken = upload(&db, &ap, broken.as_bytes()).await;
    let d = get(&db, &ap, &broken).await;
    assert_eq!(d["pages"]["count"], Value::Null);
    assert_eq!(d["pages"]["arrangement"], Value::Null);
    let e = arrange(&db, &ap, &broken, 0, order(&[1]))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("can't be read")),
        "{e}"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_bill_pins_its_arrangement_and_pages_lock_after_approval() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let ap = agent("claude-ap");
    let v = vendor(&db.state, &robert, "Acme Supply").await;
    set_workflow(
        &db.state,
        &robert,
        Some(&v),
        json!([
            { "id": "ok", "type": "approve" },
            { "id": "pay", "type": "pay", "release": "manual" }
        ]),
    )
    .await;
    let id = upload(&db, &ap, &pdf(3)).await;
    let v1 = arrange(&db, &ap, &id, 0, order(&[2, 1, 3])).await.unwrap();
    let v1_id = v1["result"]["pages"]["arrangement_id"].clone();

    // The bill pins the version it was entered from.
    let bill = commit(
        &db.state,
        &ap,
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "A-1", "invoice_date": "2027-01-15",
                "amount_cents": 10_000, "document_id": id }),
    )
    .await
    .unwrap()["result"]
        .clone();
    let bill_id = bill["bill"]["id"].as_str().unwrap().to_owned();
    assert_eq!(bill["bill"]["arrangement_id"], v1_id);
    assert_eq!(bill["arrangement"]["version"], 1);
    assert_eq!(bill["arrangement"]["pages"][0]["page"], 2);

    // Before approval, a change moves the pin, on the bill's own record.
    let v2 = arrange(
        &db,
        &ap,
        &id,
        1,
        json!([{ "page": 2 }, { "page": 1 }, { "page": 3, "hidden": true }]),
    )
    .await
    .unwrap();
    assert_eq!(
        v2["events"],
        json!(["document.arranged", "bill.pages_arranged"])
    );
    let b = commit(
        &db.state,
        &robert,
        "bill.get",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(
        b["bill"]["arrangement_id"],
        v2["result"]["pages"]["arrangement_id"]
    );
    assert_eq!(b["arrangement"]["version"], 2);
    assert_eq!(b["arrangement"]["pages"][2]["hidden"], true);
    let moved = b["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "bill.pages_arranged")
        .expect("the bill's history records the new pages");
    assert_eq!(moved["payload"]["summary"], "hid page 3");
    assert_eq!(moved["actor"], "claude-ap");
    assert_eq!(
        b["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "document.arranged")
            .count(),
        0,
        "the document's own event stays on the document"
    );

    // After the first approval the pages lock, for people and agents alike.
    commit(
        &db.state,
        &robert,
        "bill.approve",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap();
    let d = get(&db, &ap, &id).await;
    assert_eq!(d["pages"]["locked"], true);
    assert_eq!(
        d["pages"]["locked_reason"],
        "Pages are locked after approval"
    );
    for who in [&robert, &ap] {
        let e = arrange(&db, who, &id, 2, order(&[1, 2, 3]))
            .await
            .unwrap_err();
        assert!(
            matches!(&e, CommandError::Conflict(m) if m == "Pages are locked after approval"),
            "{e}"
        );
    }
    let b = commit(
        &db.state,
        &robert,
        "bill.get",
        json!({ "bill_id": bill_id }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(b["arrangement"]["version"], 2, "the pin stays where it was");
    db.drop().await;
}

#[tokio::test]
async fn a_closed_bill_or_a_trashed_document_keeps_its_pages() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "Acme Supply").await;

    // A bill from an untouched document pins nothing: the original.
    let id = upload(&db, &robert, &pdf(2)).await;
    let bill = commit(
        &db.state,
        &robert,
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "A-2", "invoice_date": "2027-01-15",
                "amount_cents": 500, "document_id": id }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(bill["bill"]["arrangement_id"], Value::Null);
    assert_eq!(bill["arrangement"]["version"], 0);
    assert_eq!(bill["arrangement"]["pages"].as_array().unwrap().len(), 2);
    let bill_id = bill["bill"]["id"].as_str().unwrap();
    // Open and unapproved, the default workflow lets the pages change.
    arrange(&db, &robert, &id, 0, order(&[2, 1])).await.unwrap();
    // Voided without any approval: locked, saying why.
    common::confirm(
        &db.state,
        &robert,
        "bill.void",
        json!({ "bill_id": bill_id, "reason": "entered twice" }),
    )
    .await
    .unwrap();
    let e = arrange(&db, &robert, &id, 1, order(&[1, 2]))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m == "Pages are locked: the bill is void"),
        "{e}"
    );

    // A document set aside as not a bill is refused too.
    let trashed = upload(&db, &robert, &pdf(2)).await;
    commit(
        &db.state,
        &robert,
        "document.trash",
        json!({ "document_id": trashed, "reason": "a statement" }),
    )
    .await
    .unwrap();
    let d = get(&db, &robert, &trashed).await;
    assert_eq!(d["pages"]["locked"], true);
    let e = arrange(&db, &robert, &trashed, 0, order(&[2, 1]))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("set aside")),
        "{e}"
    );
    db.drop().await;
}

#[tokio::test]
async fn documents_from_before_page_counts_are_counted_when_opened() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let id = upload(&db, &robert, &pdf(3)).await;
    // As a document stored before migration 0016 would be.
    let mut tx = db
        .state
        .store
        .begin(payvy_store::DEFAULT_ORG)
        .await
        .unwrap();
    sqlx::query("UPDATE documents SET page_count = NULL WHERE id = $1::uuid")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let d = get(&db, &robert, &id).await;
    assert_eq!(d["pages"]["count"], 3);
    assert_eq!(
        d["document"]["page_count"],
        Value::Null,
        "read before counting"
    );
    assert_eq!(
        get(&db, &robert, &id).await["document"]["page_count"],
        3,
        "and kept"
    );
    arrange(&db, &robert, &id, 0, order(&[3, 2, 1]))
        .await
        .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn another_organization_cant_see_or_arrange_the_pages() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let id = upload(&db, &person("robert"), &pdf(2)).await;
    arrange(&db, &person("robert"), &id, 0, order(&[2, 1]))
        .await
        .unwrap();

    let other = db.add_org().await;
    let eve = Actor::person(other, "eve");
    let e = commit(
        &db.state,
        &eve,
        "document.get",
        json!({ "document_id": id }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::NotFound(_)), "{e}");
    let e = arrange(&db, &eve, &id, 1, order(&[1, 2]))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::NotFound(_)), "{e}");

    // The arrangement rows themselves are invisible to the other
    // organization, even by a direct query under its own scope.
    let mut tx = db.state.store.begin(other).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM document_arrangements")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0);
    // ...and it can't write one for a document it doesn't own.
    let e = sqlx::query(
        "INSERT INTO document_arrangements (id, org_id, document_id, version, pages, created_at, created_by)
         VALUES (gen_random_uuid(), $1, $2::uuid, 9, '[]', now(), 'eve')",
    )
    .bind(payvy_store::DEFAULT_ORG)
    .bind(&id)
    .execute(&mut *tx)
    .await;
    assert!(e.is_err(), "row-level security refuses another org's row");
    drop(tx);

    let mut tx = db
        .state
        .store
        .begin(payvy_store::DEFAULT_ORG)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM document_arrangements")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 1);
    db.drop().await;
}

/// A model API that always reads the same invoice, as Claude's would.
async fn fake_model() -> String {
    let answer = json!({
        "kind": "invoice", "vendor_name": "Acme Supply", "vendor_email": null, "vendor_address": null,
        "account_number": null, "invoice_number": "A-9", "invoice_date": "2027-01-10",
        "due_date": null, "terms": null, "currency": "USD", "po_number": null, "subtotal": null,
        "tax": null, "total": "90.00", "lines": [], "notes": null
    });
    let reply = json!({ "model": "claude-opus-5-5", "stop_reason": "end_turn",
                        "content": [{ "type": "text", "text": answer.to_string() }] });
    let app = axum::Router::new().route(
        "/v1/messages",
        post(move || {
            let reply = reply.clone();
            async move { Json(reply) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_reading_from_before_the_pages_changed_is_out_of_date() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    let extractor = Extractor::new(Config {
        provider: Provider::Anthropic,
        base_url: fake_model().await,
        api_key: Some("test-model-key".into()),
        model: "claude-opus-5-5".into(),
    })
    .unwrap();
    db.state = db.state.clone().with_extractor(extractor);
    let robert = person("robert");
    let id = upload(&db, &robert, &pdf(2)).await;
    commit(
        &db.state,
        &robert,
        "document.extract",
        json!({ "document_id": id }),
    )
    .await
    .unwrap();
    let d = get(&db, &robert, &id).await;
    assert_eq!(d["extractions"][0]["out_of_date"], false);

    // Rearranging doesn't read it again by itself; the reading is flagged.
    arrange(&db, &robert, &id, 0, order(&[2, 1])).await.unwrap();
    let d = get(&db, &robert, &id).await;
    assert_eq!(d["extractions"].as_array().unwrap().len(), 1);
    assert_eq!(d["extractions"][0]["out_of_date"], true);

    // Reading again is current.
    db.advance(1);
    commit(
        &db.state,
        &robert,
        "document.extract",
        json!({ "document_id": id, "again": true }),
    )
    .await
    .unwrap();
    let d = get(&db, &robert, &id).await;
    assert_eq!(d["extractions"][0]["out_of_date"], false);
    assert_eq!(d["extractions"][0]["extraction"]["arrangement_version"], 1);
    assert_eq!(d["extractions"][1]["out_of_date"], true);
    db.drop().await;
}
