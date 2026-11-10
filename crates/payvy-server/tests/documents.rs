//! The inbox against a real database and a real S3-compatible store.

use base64::Engine;
use payvy_server::commands::{self, CommandError, Mode};
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, commit, person, vendor};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A minimal PDF, different per call so tests don't collide on the hash.
fn pdf(tag: &str) -> Vec<u8> {
    format!(
        "%PDF-1.4\n% invoice {tag} {}\n%%EOF\n",
        uuid::Uuid::new_v4()
    )
    .into_bytes()
}

async fn upload(
    db: &TestDb,
    who: &payvy_server::Actor,
    name: &str,
    bytes: &[u8],
) -> Result<Value, CommandError> {
    commit(
        &db.state,
        who,
        "document.upload",
        json!({ "filename": name, "content_base64": b64(bytes) }),
    )
    .await
}

#[tokio::test]
async fn upload_file_and_download() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let ap = agent("claude-ap");
    let file = pdf("189727");

    let doc = upload(&db, &ap, "../King Environmental 189727.pdf", &file)
        .await
        .unwrap()["result"]
        .clone();
    assert_eq!(doc["status"], "inbox");
    assert_eq!(doc["content_type"], "application/pdf");
    assert_eq!(doc["filename"], "King Environmental 189727.pdf");
    assert!(
        doc.get("storage_key").is_none(),
        "the bucket layout isn't exposed"
    );
    let id = doc["id"].as_str().unwrap().to_owned();

    // The same bytes again, under any name, are caught.
    let e = upload(&db, &ap, "copy.pdf", &file).await.unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains(&id)),
        "{e}"
    );

    // The download link works without credentials and returns the original.
    let got = commit(&db.state, &ap, "document.get", json!({ "document_id": id }))
        .await
        .unwrap();
    let link = got["result"]["download_url"].as_str().unwrap();
    assert_eq!(
        reqwest::get(link)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        file.as_slice()
    );

    // Filed with a bill, it leaves the inbox and shows on the bill.
    let v = vendor(&db.state, &ap, "King Environmental").await;
    let bill = commit(
        &db.state,
        &ap,
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "189727",
        "invoice_date": "2027-01-15", "amount_cents": 42_500, "document_id": id }),
    )
    .await
    .unwrap();
    assert_eq!(bill["result"]["documents"][0]["id"], id.as_str());
    assert_eq!(bill["result"]["documents"][0]["status"], "filed");
    let inbox = commit(&db.state, &ap, "document.list", json!({}))
        .await
        .unwrap();
    assert_eq!(inbox["result"]["documents"], json!([]));

    // It can't be filed twice, and a filed document can't be trashed.
    let e = commit(
        &db.state,
        &ap,
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "189728",
        "invoice_date": "2027-01-15", "amount_cents": 100, "document_id": id }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    let e = commit(
        &db.state,
        &ap,
        "document.trash",
        json!({ "document_id": id, "reason": "x" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)));
    // ...and the failed bill.create rolled back whole: no second bill.
    let bills = commit(&db.state, &ap, "bill.list", json!({}))
        .await
        .unwrap();
    assert_eq!(bills["result"]["bills"].as_array().unwrap().len(), 1);
    db.drop().await;
}

#[tokio::test]
async fn type_is_read_from_the_bytes_and_preview_writes_nothing() {
    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let robert = person("robert");
    let e = upload(&db, &robert, "invoice.pdf", b"<html>not a pdf</html>")
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    let e = commit(
        &db.state,
        &robert,
        "document.upload",
        json!({ "filename": "x.pdf", "content_base64": "%%%" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)));

    // A photo is fine.
    let jpeg = [
        &[0xFF, 0xD8, 0xFF, 0xE0][..],
        uuid::Uuid::new_v4().as_bytes(),
    ]
    .concat();
    let photo = upload(&db, &robert, "IMG_0042.JPG", &jpeg).await.unwrap();
    assert_eq!(photo["result"]["content_type"], "image/jpeg");

    // Preview: no row, so the real upload afterwards isn't a "duplicate".
    let file = pdf("preview");
    let input = json!({ "filename": "p.pdf", "content_base64": b64(&file) });
    commands::run(
        &db.state,
        &robert,
        "document.upload",
        input.clone(),
        Mode::Preview,
    )
    .await
    .unwrap();
    commit(&db.state, &robert, "document.upload", input)
        .await
        .unwrap();

    // Trashing takes it out of the inbox with the reason.
    let list = commit(&db.state, &robert, "document.list", json!({}))
        .await
        .unwrap();
    let first = list["result"]["documents"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let t = commit(
        &db.state,
        &robert,
        "document.trash",
        json!({ "document_id": first, "reason": "a statement" }),
    )
    .await
    .unwrap();
    assert_eq!(t["result"]["status"], "trashed");
    let trashed = commit(
        &db.state,
        &robert,
        "document.list",
        json!({ "status": "trashed" }),
    )
    .await
    .unwrap();
    assert_eq!(
        trashed["result"]["documents"][0]["trashed_reason"],
        "a statement"
    );
    db.drop().await;
}

#[tokio::test]
async fn without_storage_documents_say_so() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let e = upload(&db, &person("robert"), "a.pdf", &pdf("x"))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::NotConfigured(_)), "{e}");
    db.drop().await;
}

#[tokio::test]
async fn the_web_app_reads_a_file_through_the_server() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let Some(db) = TestDb::with_storage().await else {
        return;
    };
    let bytes = pdf("viewer");
    let up = upload(&db, &agent("claude-ap"), "inv.pdf", &bytes)
        .await
        .unwrap();
    let id = up["result"]["id"].as_str().unwrap().to_owned();

    let get = |key: &'static str, id: String| {
        Request::get(format!("/api/documents/{id}/file"))
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap()
    };
    let res = payvy_server::router(db.state.clone())
        .oneshot(get(common::ADMIN_KEY, id.clone()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "application/pdf");
    assert_eq!(res.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], &bytes[..]);

    // Another organization can't read it.
    let other = db.add_org().await;
    let (key, hash) = payvy_server::new_api_key().unwrap();
    db.state
        .store
        .insert_api_key(other, "eve", payvy_store::ActorKind::Person, &hash)
        .await
        .unwrap();
    let key: &'static str = Box::leak(key.into_boxed_str());
    let res = payvy_server::router(db.state.clone())
        .oneshot(get(key, id))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    db.drop().await;
}
