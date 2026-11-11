//! Splitting a document's pages into new documents, against a real database
//! (with row-level security) and a real S3-compatible store: the checks and
//! stale versions, lineage over the one unchanged original, undo, a bill
//! from some pages in one change, and the reader's suggested invoices, which
//! an agent's reading splits on straight away and a person's only suggests.

use std::sync::{Arc, Mutex};

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

async fn upload_named(
    db: &TestDb,
    who: &Actor,
    bytes: &[u8],
    name: &str,
) -> Result<Value, CommandError> {
    commit(
        &db.state,
        who,
        "document.upload",
        json!({ "filename": name,
                "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes) }),
    )
    .await
}

async fn upload(db: &TestDb, who: &Actor, bytes: &[u8]) -> String {
    upload_named(db, who, bytes, "waste.pdf").await.unwrap()["result"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn get(db: &TestDb, who: &Actor, id: &str) -> Value {
    commit(&db.state, who, "document.get", json!({ "document_id": id }))
        .await
        .unwrap()["result"]
        .clone()
}

async fn split(
    db: &TestDb,
    who: &Actor,
    id: &str,
    expected_version: i64,
    groups: Value,
) -> Result<Value, CommandError> {
    commit(
        &db.state,
        who,
        "document.split",
        json!({ "document_id": id, "expected_version": expected_version, "groups": groups }),
    )
    .await
}

async fn unsplit(db: &TestDb, who: &Actor, id: &str) -> Result<Value, CommandError> {
    commit(
        &db.state,
        who,
        "document.unsplit",
        json!({ "document_id": id }),
    )
    .await
}

/// The original's page numbers in a document's current arrangement.
fn numbers(d: &Value) -> Vec<u64> {
    d["pages"]["arrangement"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["page"].as_u64().unwrap())
        .collect()
}

fn summaries(d: &Value, kind: &str) -> Vec<String> {
    d["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == kind)
        .map(|e| e["payload"]["summary"].as_str().unwrap().to_owned())
        .collect()
}

async fn inbox_ids(db: &TestDb, who: &Actor, status: &str) -> Vec<String> {
    commit(&db.state, who, "document.list", json!({ "status": status }))
        .await
        .unwrap()["result"]["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_split_is_checked_and_refused_on_a_stale_version() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let id = upload(&db, &robert, &pdf(6)).await;
    // Page 2 hidden, page 5 turned: version 1.
    commit(
        &db.state,
        &robert,
        "document.arrange",
        json!({ "document_id": id, "expected_version": 0, "pages": [
            { "page": 1 }, { "page": 2, "hidden": true }, { "page": 3 },
            { "page": 4 }, { "page": 5, "rotation": 90 }, { "page": 6 }
        ] }),
    )
    .await
    .unwrap();

    for (groups, says) in [
        (json!([]), "at least one group"),
        (json!([[]]), "group 1 is empty"),
        (json!([[1, 3], [3, 4]]), "page 3 is in more than one group"),
        (json!([[4, 4]]), "page 4 is in more than one group"),
        (json!([[7]]), "page 7 isn't in this document"),
        (json!([[2]]), "only hidden pages"),
    ] {
        let e = split(&db, &robert, &id, 1, groups).await.unwrap_err();
        assert!(
            matches!(&e, CommandError::Invalid(m) if m.contains(says)),
            "{says}: {e}"
        );
    }
    let e = split(&db, &robert, &id, 0, json!([[5, 6]]))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("version 1")),
        "{e}"
    );

    // Pages 5–6 into a new document, keeping page 5's turn.
    let r = split(&db, &robert, &id, 1, json!([[5, 6]])).await.unwrap();
    assert_eq!(
        r["events"],
        json!(["document.split_from", "document.split"])
    );
    let source = &r["result"];
    assert_eq!(source["pages"]["version"], 2);
    assert_eq!(numbers(source), [1, 2, 3, 4]);
    assert_eq!(source["pages"]["arrangement"][1]["hidden"], true);
    assert_eq!(source["document"]["status"], "inbox");
    let part = &r["result"]["documents"][0];
    assert_eq!(part["filename"], "waste (pages 5–6).pdf");

    // A moved page is gone from the source, not hidden: it can't be split
    // or arranged there again.
    let e = split(&db, &robert, &id, 2, json!([[5]])).await.unwrap_err();
    assert!(
        matches!(&e, CommandError::Invalid(m) if m.contains("page 5 isn't in this document")),
        "{e}"
    );
    let e = commit(
        &db.state,
        &robert,
        "document.arrange",
        json!({ "document_id": id, "expected_version": 2,
                "pages": [{ "page": 1 }, { "page": 2 }, { "page": 3 }, { "page": 4 }, { "page": 5 }] }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&e, CommandError::Invalid(m) if m.contains("page 5 isn't in this document")),
        "{e}"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_split_document_shares_the_original_and_shows_only_its_pages() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let file = pdf(6);
    let id = upload(&db, &robert, &file).await;
    let r = split(&db, &robert, &id, 0, json!([[6, 5]])).await.unwrap();
    let part_id = r["result"]["documents"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let part = get(&db, &robert, &part_id).await;
    let original = get(&db, &robert, &id).await;
    // Lineage, and the same stored file: nothing was copied.
    assert_eq!(part["document"]["derived_from"], json!(id));
    assert_eq!(part["split_from"]["id"], json!(id));
    assert_eq!(part["split_from"]["filename"], "waste.pdf");
    assert_eq!(part["document"]["sha256"], original["document"]["sha256"]);
    assert_eq!(part["document"]["created_by"], "robert");
    let got = reqwest::get(part["download_url"].as_str().unwrap())
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(got.as_ref(), file.as_slice(), "the original, unchanged");
    // Its pages: only 6 and 5, in the order given, as version 1. The page
    // count is the file's.
    assert_eq!(numbers(&part), [6, 5]);
    assert_eq!(part["pages"]["version"], 1);
    assert_eq!(part["pages"]["count"], 6);
    assert_eq!(numbers(&original), [1, 2, 3, 4]);
    assert_eq!(original["split_into"][0]["id"], json!(part_id));
    assert_eq!(original["split_into"][0]["pages"], json!([6, 5]));

    // It can be arranged among its own pages only.
    let arrange = |pages: Value| {
        commit(
            &db.state,
            &robert,
            "document.arrange",
            json!({ "document_id": part_id, "expected_version": 1, "pages": pages }),
        )
    };
    let e = arrange(json!([{ "page": 5 }, { "page": 6 }, { "page": 1 }]))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Invalid(m) if m.contains("page 1 isn't in this document")),
        "{e}"
    );
    let e = arrange(json!([{ "page": 5 }])).await.unwrap_err();
    assert!(
        matches!(&e, CommandError::Invalid(m) if m.contains("page 6 is missing")),
        "{e}"
    );
    arrange(json!([{ "page": 5 }, { "page": 6 }]))
        .await
        .unwrap();

    // Both histories say what happened, in words.
    assert_eq!(
        summaries(&original, "document.split"),
        ["split pages 6 and 5 into a new document"]
    );
    assert_eq!(
        summaries(&part, "document.split_from"),
        ["split this from waste.pdf (pages 6 and 5)"]
    );
    // The inbox lists it with where it came from.
    let listed = commit(&db.state, &robert, "document.list", json!({}))
        .await
        .unwrap()["result"]["documents"]
        .clone();
    let row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == json!(part_id))
        .unwrap();
    assert_eq!(row["split_from"]["filename"], "waste.pdf");
    db.drop().await;
}

#[tokio::test]
async fn a_split_is_undone_by_people_and_agents() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let ap = agent("claude-ap");
    let id = upload(&db, &robert, &pdf(6)).await;

    // Every page into two documents: the source has none left.
    let r = split(&db, &ap, &id, 0, json!([[1, 2, 3], [4, 5, 6]]))
        .await
        .unwrap();
    assert_eq!(r["result"]["document"]["status"], "split");
    assert_eq!(numbers(&r["result"]), Vec::<u64>::new());
    assert_eq!(
        summaries(&r["result"], "document.split"),
        ["split pages 1–3 and pages 4–6 into 2 new documents"]
    );
    let first = r["result"]["documents"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = r["result"]["documents"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let inbox = inbox_ids(&db, &robert, "inbox").await;
    assert!(!inbox.contains(&id), "it left To review");
    assert!(inbox.contains(&first) && inbox.contains(&second));
    assert_eq!(
        inbox_ids(&db, &robert, "split").await,
        std::slice::from_ref(&id)
    );
    let e = split(&db, &ap, &id, 1, json!([[1]])).await.unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    // The agent undoes the second: its pages go back, in place.
    let r = unsplit(&db, &ap, &second).await.unwrap();
    assert_eq!(r["events"], json!(["document.unsplit", "document.unsplit"]));
    assert_eq!(r["result"]["document"]["status"], "inbox");
    assert_eq!(numbers(&r["result"]), [4, 5, 6]);
    let gone = get(&db, &robert, &second).await;
    assert_eq!(gone["document"]["status"], "split");
    assert_eq!(numbers(&gone), Vec::<u64>::new());
    assert_eq!(
        summaries(&gone, "document.unsplit"),
        ["gave pages 4–6 back to waste.pdf"]
    );

    // Robert undoes the first: the original order again.
    let r = unsplit(&db, &robert, &first).await.unwrap();
    assert_eq!(numbers(&r["result"]), [1, 2, 3, 4, 5, 6]);
    assert_eq!(
        summaries(&r["result"], "document.unsplit"),
        [
            "took back pages 1–3 from waste (pages 1–3).pdf",
            "took back pages 4–6 from waste (pages 4–6).pdf"
        ]
    );
    let actors: Vec<&str> = r["result"]["history"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "document.unsplit")
        .map(|e| e["actor"].as_str().unwrap())
        .collect();
    assert_eq!(actors, ["robert", "claude-ap"]);
    assert_eq!(
        inbox_ids(&db, &robert, "inbox").await,
        std::slice::from_ref(&id)
    );

    // Nothing left to undo, and an original has nothing to undo.
    let e = unsplit(&db, &robert, &first).await.unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    let e = unsplit(&db, &robert, &id).await.unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn a_bill_from_some_pages_splits_them_off_in_the_same_change() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let v = vendor(&db.state, &robert, "American Reclamation").await;
    let id = upload(&db, &robert, &pdf(6)).await;
    let create = |number: &str, pages: Value, version: i64| {
        commit(
            &db.state,
            &robert,
            "bill.create",
            json!({ "vendor_id": v, "invoice_number": number, "invoice_date": "2027-01-15",
                    "amount_cents": 803_489, "document_id": id, "pages": pages,
                    "expected_version": version }),
        )
    };

    // Invoice 85273 is pages 1–3.
    let b = create("85273", json!([1, 2, 3]), 0).await.unwrap();
    assert_eq!(
        b["events"],
        json!([
            "document.split_from",
            "document.split",
            "bill.created",
            "document.filed"
        ])
    );
    let bill = &b["result"];
    let filed = &bill["documents"][0];
    assert_ne!(filed["id"], json!(id), "a new document is filed");
    assert_eq!(filed["derived_from"], json!(id));
    assert_eq!(bill["arrangement"]["version"], 1);
    assert_eq!(bill["arrangement"]["pages"].as_array().unwrap().len(), 3);
    assert_eq!(
        bill["bill"]["arrangement_id"],
        bill["arrangement"]["arrangement_id"]
    );
    let created = bill["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "bill.created")
        .unwrap();
    assert_eq!(created["payload"]["split_from"], json!(id));
    // The rest stays in To review, as the original's next version.
    let rest = get(&db, &robert, &id).await;
    assert_eq!(rest["document"]["status"], "inbox");
    assert_eq!(numbers(&rest), [4, 5, 6]);
    assert_eq!(rest["pages"]["version"], 1);

    // A bill that is refused leaves no split behind: a duplicate invoice
    // number, a stale version, or pages the document doesn't have.
    let before = inbox_ids(&db, &robert, "inbox").await;
    let e = create("85273", json!([4]), 1).await.unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("possible duplicate")),
        "{e}"
    );
    let e = create("85274", json!([4]), 0).await.unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("version 0")),
        "{e}"
    );
    let e = create("85274", json!([2]), 1).await.unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    assert_eq!(inbox_ids(&db, &robert, "inbox").await, before);
    let rest = get(&db, &robert, &id).await;
    assert_eq!(rest["pages"]["version"], 1);
    assert_eq!(rest["split_into"].as_array().unwrap().len(), 1);

    // Every page left: the document itself is filed, as without pages.
    let b = create("85275", json!([4, 5, 6]), 1).await.unwrap();
    assert_eq!(b["events"], json!(["bill.created", "document.filed"]));
    assert_eq!(b["result"]["documents"][0]["id"], json!(id));
    let e = commit(
        &db.state,
        &robert,
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "X", "invoice_date": "2027-01-15",
                "amount_cents": 1, "pages": [1] }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn locked_and_set_aside_documents_keep_their_pages() {
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

    // A document split off and then filed can't be undone.
    let id = upload(&db, &ap, &pdf(4)).await;
    let r = split(&db, &ap, &id, 0, json!([[3, 4]])).await.unwrap();
    let part = r["result"]["documents"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    commit(
        &db.state,
        &ap,
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "P-1", "invoice_date": "2027-01-15",
                "amount_cents": 100, "document_id": part }),
    )
    .await
    .unwrap();
    let e = unsplit(&db, &ap, &part).await.unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("filed with bill")),
        "{e}"
    );

    // The source filed and approved: locked, so nothing splits off it and
    // nothing comes back to it.
    let r = split(&db, &ap, &id, 1, json!([[2]])).await.unwrap();
    let back = r["result"]["documents"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let bill = commit(
        &db.state,
        &ap,
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "P-2", "invoice_date": "2027-01-15",
                "amount_cents": 100, "document_id": id }),
    )
    .await
    .unwrap()["result"]["bill"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    commit(
        &db.state,
        &robert,
        "bill.approve",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap();
    for who in [&robert, &ap] {
        let e = split(&db, who, &id, 2, json!([[1]])).await.unwrap_err();
        assert!(
            matches!(&e, CommandError::Conflict(m) if m == "Pages are locked after approval"),
            "{e}"
        );
        let e = unsplit(&db, who, &back).await.unwrap_err();
        assert!(
            matches!(&e, CommandError::Conflict(m) if m.contains("locked after approval")),
            "{e}"
        );
    }

    // Set aside as not a bill: refused, and a set-aside part stays put.
    let trashed = upload(&db, &robert, &pdf(2)).await;
    let r = split(&db, &robert, &trashed, 0, json!([[2]]))
        .await
        .unwrap();
    let trashed_part = r["result"]["documents"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for doc in [&trashed, &trashed_part] {
        commit(
            &db.state,
            &robert,
            "document.trash",
            json!({ "document_id": doc, "reason": "a statement" }),
        )
        .await
        .unwrap();
    }
    let e = split(&db, &robert, &trashed, 1, json!([[1]]))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("set aside")),
        "{e}"
    );
    let e = unsplit(&db, &robert, &trashed_part).await.unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("set aside")),
        "{e}"
    );
    db.drop().await;
}

#[tokio::test]
async fn the_same_file_again_is_still_refused_after_a_split() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let file = pdf(3);
    let id = upload(&db, &robert, &file).await;
    // Splitting makes documents with the same hash, which is allowed...
    split(&db, &robert, &id, 0, json!([[1], [2]]))
        .await
        .unwrap();
    // ...but the file arriving again is a copy, as before, and names the
    // document it arrived as.
    let e = upload_named(&db, &robert, &file, "again.pdf")
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("this exact file is already here")
            && m.contains(&id) && m.contains("waste.pdf")),
        "{e}"
    );
    // Still, once every page has moved.
    split(&db, &robert, &id, 1, json!([[3]])).await.unwrap();
    let e = upload_named(&db, &robert, &file, "again.pdf")
        .await
        .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("split into other documents")),
        "{e}"
    );
    // A different file is welcome.
    upload_named(&db, &robert, &pdf(3), "other.pdf")
        .await
        .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn another_organization_cant_split_undo_or_see_the_parts() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let id = upload(&db, &robert, &pdf(3)).await;
    let r = split(&db, &robert, &id, 0, json!([[3]])).await.unwrap();
    let part = r["result"]["documents"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let other = db.add_org().await;
    let eve = Actor::person(other, "eve");
    let e = split(&db, &eve, &id, 1, json!([[1]])).await.unwrap_err();
    assert!(matches!(e, CommandError::NotFound(_)), "{e}");
    let e = unsplit(&db, &eve, &part).await.unwrap_err();
    assert!(matches!(e, CommandError::NotFound(_)), "{e}");
    assert!(inbox_ids(&db, &eve, "inbox").await.is_empty());

    // Even by a direct query under its own scope, the parts are invisible,
    // and it can't make one of its own from another's document.
    let mut tx = db.state.store.begin(other).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM documents")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let e = sqlx::query(
        "INSERT INTO documents (id, org_id, sha256, content_type, size_bytes, filename, storage_key,
                                source, received_at, created_by, derived_from)
         VALUES (gen_random_uuid(), $1, 'x', 'application/pdf', 1, 'x.pdf', 'k', 'upload', now(), 'eve', $2::uuid)",
    )
    .bind(other)
    .bind(&id)
    .execute(&mut *tx)
    .await;
    assert!(e.is_err(), "lineage stays within one organization");
    drop(tx);
    db.drop().await;
}

// ── The reader's suggested invoices ─────────────────────────────────────────

/// What a fake model was asked, in order.
type Asked = Arc<Mutex<Vec<String>>>;

fn invoice(number: &str, total: &str, invoices: Value) -> Value {
    json!({
        "kind": "invoice", "vendor_name": "American Reclamation", "vendor_email": null,
        "vendor_address": null, "account_number": null, "invoice_number": number,
        "invoice_date": "2027-01-10", "due_date": null, "terms": null, "currency": "USD",
        "po_number": null, "subtotal": null, "tax": null, "total": total, "lines": [],
        "notes": null, "invoices": invoices
    })
}

/// A model that reads a three-page file as two invoices (pages 1–2 and 3),
/// and each part as the one invoice on the pages it was told to read.
async fn fake_model(many: bool) -> (Asked, String) {
    let asked: Asked = Arc::default();
    let log = asked.clone();
    let app = axum::Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                let prompt = body["messages"][0]["content"][1]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                log.lock().unwrap().push(prompt.clone());
                let answer = if many {
                    // One "invoice" per page of a 21-page file.
                    let each: Vec<Value> = (1..=21)
                        .map(|n| json!({ "pages": [n], "invoice_number": format!("9{n:03}"), "total": "1.00" }))
                        .collect();
                    invoice("9001", "1.00", json!(each))
                } else if prompt.contains("Read only pages 1, 2 of") {
                    invoice("85273", "8,034.89", json!([]))
                } else if prompt.contains("Read only page 3 of") {
                    // A part may still look like two to the model; it is
                    // never split again.
                    invoice(
                        "85274",
                        "1,200.00",
                        json!([
                            { "pages": [3], "invoice_number": "85274", "total": "1,200.00" },
                            { "pages": [3], "invoice_number": "85274", "total": "1,200.00" }
                        ]),
                    )
                } else {
                    invoice(
                        "85273",
                        "8,034.89",
                        json!([
                            { "pages": [1, 2], "invoice_number": "85273", "total": "8,034.89" },
                            { "pages": [3], "invoice_number": "85274", "total": "1,200.00" }
                        ]),
                    )
                };
                Json(
                    json!({ "model": "claude-opus-5-5", "stop_reason": "end_turn",
                             "content": [{ "type": "text", "text": answer.to_string() }] }),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (asked, format!("http://{addr}"))
}

async fn with_model(db: &mut TestDb) -> Asked {
    with_fake(db, false).await
}

async fn with_fake(db: &mut TestDb, many: bool) -> Asked {
    let (asked, url) = fake_model(many).await;
    let extractor = Extractor::new(Config {
        provider: Provider::Anthropic,
        base_url: url,
        api_key: Some("test-model-key".into()),
        model: "claude-opus-5-5".into(),
    })
    .unwrap();
    db.state = db.state.clone().with_extractor(extractor);
    asked
}

#[tokio::test]
async fn the_reader_splits_a_file_of_two_invoices_and_reads_each_once() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    let asked = with_model(&mut db).await;
    let robert = person("robert");
    vendor(&db.state, &robert, "American Reclamation").await;
    let id = upload(&db, &agent("email"), &pdf(3)).await;

    // One pass: the file is read, split in two, and each part read.
    assert_eq!(
        payvy_server::extract_worker::tick(&db.state).await.unwrap(),
        3
    );
    // ...and the next pass has nothing to do: no part is split again.
    assert_eq!(
        payvy_server::extract_worker::tick(&db.state).await.unwrap(),
        0
    );
    let prompts = asked.lock().unwrap().clone();
    assert_eq!(prompts.len(), 3, "{prompts:#?}");
    assert_eq!(prompts[0], "Read this document.");
    assert!(prompts[1].contains("Read only pages 1, 2 of this file"));
    assert!(prompts[2].contains("Read only page 3 of this file"));

    let source = get(&db, &robert, &id).await;
    assert_eq!(source["document"]["status"], "split", "left To review");
    let reading = &source["extractions"][0];
    assert_eq!(reading["invoices"].as_array().unwrap().len(), 2);
    assert_eq!(reading["invoices"][0]["pages"], json!([1, 2]));
    assert_eq!(reading["invoices"][0]["total_cents"], 803_489);
    assert!(
        reading["failed_checks"]
            .as_array()
            .unwrap()
            .contains(&json!("one_invoice"))
    );
    let split_event = source["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "document.split")
        .unwrap();
    assert_eq!(split_event["actor"], "extractor", "the reader's name");
    assert_eq!(
        split_event["payload"]["summary"],
        "split pages 1–2 and page 3 into 2 new documents"
    );

    let parts = source["split_into"].as_array().unwrap().clone();
    assert_eq!(parts.len(), 2);
    let first = get(&db, &robert, parts[0]["id"].as_str().unwrap()).await;
    assert_eq!(numbers(&first), [1, 2]);
    assert_eq!(first["document"]["created_by"], "extractor");
    let read = &first["extractions"][0];
    assert_eq!(read["extraction"]["created_by"], "extractor");
    assert_eq!(read["ready"], true, "{read:#}");
    assert_eq!(read["bill_input"]["invoice_number"], "85273");
    assert_eq!(read["bill_input"]["document_id"], parts[0]["id"]);
    // The second part read as two, which a part can't be: the reading
    // fails the check, and nothing is split.
    let second = get(&db, &robert, parts[1]["id"].as_str().unwrap()).await;
    assert_eq!(second["document"]["status"], "inbox");
    assert!(second["split_into"].as_array().unwrap().is_empty());
    assert_eq!(second["extractions"][0]["ready"], false);

    // An agent reading again through the API is a reader too: an original
    // splits and says what to read next. Undo brings the pages back.
    unsplit(&db, &robert, parts[0]["id"].as_str().unwrap())
        .await
        .unwrap();
    unsplit(&db, &robert, parts[1]["id"].as_str().unwrap())
        .await
        .unwrap();
    db.advance(1);
    let r = commit(
        &db.state,
        &agent("claude-ap"),
        "document.extract",
        json!({ "document_id": id, "again": true }),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["split"]["to_read"].as_array().unwrap().len(), 2);
    assert!(
        r["events"]
            .as_array()
            .unwrap()
            .contains(&json!("document.split"))
    );
    db.drop().await;
}

#[tokio::test]
async fn a_persons_reading_only_suggests_the_invoices() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    with_model(&mut db).await;
    let robert = person("robert");
    let id = upload(&db, &robert, &pdf(3)).await;
    let r = commit(
        &db.state,
        &robert,
        "document.extract",
        json!({ "document_id": id }),
    )
    .await
    .unwrap();
    assert_eq!(r["events"], json!(["document.extracted"]));
    assert!(r["result"].get("split").is_none());
    assert_eq!(r["result"]["invoices"][1]["pages"], json!([3]));
    assert_eq!(r["result"]["invoices"][1]["invoice_number"], "85274");
    let d = get(&db, &robert, &id).await;
    assert_eq!(d["document"]["status"], "inbox");
    assert_eq!(numbers(&d), [1, 2, 3]);
    assert!(d["split_into"].as_array().unwrap().is_empty());
    let check = d["extractions"][0]["extraction"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["check"] == "one_invoice")
        .unwrap()
        .clone();
    assert_eq!(
        check["detail"],
        "this file holds 2 invoices: invoice 85273 on pages 1–2; invoice 85274 on page 3"
    );
    db.drop().await;
}

#[tokio::test]
async fn the_reader_leaves_a_file_of_too_many_invoices_for_a_person() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    let asked = with_fake(&mut db, true).await;
    let robert = person("robert");
    let id = upload(&db, &agent("email"), &pdf(21)).await;

    // Read once, not split: 21 parts would mean 21 more model calls.
    assert_eq!(
        payvy_server::extract_worker::tick(&db.state).await.unwrap(),
        1
    );
    assert_eq!(asked.lock().unwrap().len(), 1);
    let d = get(&db, &robert, &id).await;
    assert_eq!(d["document"]["status"], "inbox");
    assert!(d["split_into"].as_array().unwrap().is_empty());
    assert_eq!(numbers(&d).len(), 21);
    db.drop().await;
}
