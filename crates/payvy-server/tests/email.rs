//! Email intake against a real database and a real S3-compatible store:
//! the inbound address, the signed webhook, and email.import.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use payvy_server::commands::CommandError;
use payvy_server::inbound::Inbound;
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::{TestDb, agent, commit, person};

const DOMAIN: &str = "in.example.com";
const SECRET: &str = "test-inbound-secret-0123456789abcdef";

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn db() -> Option<TestDb> {
    let mut db = TestDb::with_storage().await?;
    db.state = db
        .state
        .clone()
        .with_inbound(Inbound::new(DOMAIN, SECRET).unwrap());
    Some(db)
}

/// An email from King Environmental with the given PDFs attached. Each call
/// has its own Message-ID unless one is given.
fn email(to: &str, message_id: Option<&str>, pdfs: &[(&str, &[u8])]) -> Vec<u8> {
    let id = message_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}@kingenv.com", uuid::Uuid::new_v4()));
    let mut s = format!(
        "From: King Environmental <ar@kingenv.com>\r\nTo: {to}\r\nSubject: Invoice 189727\r\n\
         Message-ID: <{id}>\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
         --b\r\nContent-Type: text/plain\r\n\r\nAttached.\r\n"
    );
    for (name, bytes) in pdfs {
        s += &format!(
            "--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            b64(bytes)
        );
    }
    s += "--b--\r\n";
    s.into_bytes()
}

fn pdf() -> Vec<u8> {
    format!("%PDF-1.4\n% {}\n%%EOF\n", uuid::Uuid::new_v4()).into_bytes()
}

async fn post(
    db: &TestDb,
    recipient: &str,
    body: &[u8],
    ts: i64,
    sig: Option<String>,
) -> (StatusCode, Value) {
    let sig = sig.unwrap_or_else(|| {
        Inbound::new(DOMAIN, SECRET)
            .unwrap()
            .sign(ts, recipient, body)
    });
    let req = Request::post("/webhooks/email")
        .header("x-payvy-timestamp", ts.to_string())
        .header("x-payvy-recipient", recipient)
        .header("x-payvy-signature", sig)
        .body(Body::from(body.to_vec()))
        .unwrap();
    let res = payvy_server::router(db.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn now(db: &TestDb) -> i64 {
    db.clock.lock().unwrap().timestamp()
}

async fn new_address(db: &TestDb) -> String {
    let out = commit(&db.state, &person("robert"), "email.new_address", json!({}))
        .await
        .unwrap();
    out["result"]["address"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn address_is_made_and_replaced_by_a_person() {
    let Some(db) = db().await else { return };
    let ap = agent("claude-ap");

    let none = commit(&db.state, &ap, "email.address", json!({}))
        .await
        .unwrap();
    assert!(none["result"]["address"].is_null());
    let e = commit(&db.state, &ap, "email.new_address", json!({}))
        .await
        .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    let first = new_address(&db).await;
    assert!(first.ends_with("@in.example.com"), "{first}");
    let shown = commit(&db.state, &ap, "email.address", json!({}))
        .await
        .unwrap();
    assert_eq!(shown["result"]["address"], first.as_str());

    let second = commit(&db.state, &person("robert"), "email.new_address", json!({}))
        .await
        .unwrap();
    assert_eq!(second["result"]["retired"], first.as_str());
    assert_ne!(second["result"]["address"], first.as_str());

    // The retired address no longer routes anywhere.
    let raw = email(&first, None, &[("a.pdf", &pdf())]);
    let (status, body) = post(&db, &first, &raw, now(&db), None).await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::OK, Some("ignored"))
    );
    db.drop().await;
}

#[tokio::test]
async fn webhook_imports_attachments_once() {
    let Some(db) = db().await else { return };
    let to = new_address(&db).await;
    let robert = person("robert");

    // One attachment is already in the inbox from an upload.
    let (known, fresh) = (pdf(), pdf());
    let up = commit(
        &db.state,
        &robert,
        "document.upload",
        json!({ "filename": "known.pdf", "content_base64": b64(&known) }),
    )
    .await
    .unwrap();
    let known_id = up["result"]["id"].as_str().unwrap().to_owned();

    let tagged = to.replace('@', "+fuel@");
    let raw = email(
        &tagged,
        None,
        &[("INV-189727.pdf", &fresh), ("known.pdf", &known)],
    );
    let (status, body) = post(&db, &tagged, &raw, now(&db), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "imported");
    let result = &body["result"];
    assert_eq!(result["email"]["from_address"], "ar@kingenv.com");
    assert_eq!(result["email"]["subject"], "Invoice 189727");
    assert_eq!(result["email"]["created_by"], "email");
    let docs = result["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["filename"], "INV-189727.pdf");
    assert_eq!(docs[0]["source"], "email");
    assert_eq!(docs[0]["status"], "inbox");
    let skipped = result["email"]["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert!(
        skipped[0]["reason"].as_str().unwrap().contains(&known_id),
        "{skipped:?}"
    );

    // A retry of the same delivery is a duplicate, and says so with a 200 so
    // the worker stops.
    let (status, body) = post(&db, &tagged, &raw, now(&db), None).await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::OK, Some("duplicate"))
    );

    // The inbox has both files once each; the original email can be fetched.
    let inbox = commit(&db.state, &robert, "document.list", json!({}))
        .await
        .unwrap();
    assert_eq!(inbox["result"]["documents"].as_array().unwrap().len(), 2);
    let email_id = result["email"]["id"].as_str().unwrap();
    let got = commit(
        &db.state,
        &robert,
        "email.get",
        json!({ "email_id": email_id }),
    )
    .await
    .unwrap();
    let link = got["result"]["download_url"].as_str().unwrap();
    let original = reqwest::get(link).await.unwrap().bytes().await.unwrap();
    assert_eq!(original.as_ref(), raw.as_slice());
    db.drop().await;
}

#[tokio::test]
async fn webhook_refuses_unsigned_stale_and_garbage() {
    let Some(db) = db().await else { return };
    let to = new_address(&db).await;
    let raw = email(&to, None, &[("a.pdf", &pdf())]);
    let ts = now(&db);

    let (status, _) = post(&db, &to, &raw, ts, Some("00".repeat(32))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Signed for one recipient, replayed to another.
    let other = Inbound::new(DOMAIN, SECRET)
        .unwrap()
        .sign(ts, "someone@in.example.com", &raw);
    let (status, _) = post(&db, &to, &raw, ts, Some(other)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = post(&db, &to, &raw, ts - 600, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = post(&db, "nobody@in.example.com", &raw, ts, None).await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::OK, Some("ignored"))
    );
    let (status, body) = post(&db, &to, b"", ts, None).await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::OK, Some("rejected"))
    );

    let listed = commit(&db.state, &person("robert"), "email.list", json!({}))
        .await
        .unwrap();
    assert!(listed["result"]["emails"].as_array().unwrap().is_empty());
    db.drop().await;
}

#[tokio::test]
async fn agent_imports_an_eml_and_orgs_stay_apart() {
    let Some(db) = db().await else { return };
    let ap = agent("claude-ap");
    let raw = email(
        "ap@quarry.example",
        Some("fixed@kingenv.com"),
        &[("a.pdf", &pdf())],
    );

    let out = commit(
        &db.state,
        &ap,
        "email.import",
        json!({ "raw_base64": b64(&raw) }),
    )
    .await
    .unwrap();
    assert_eq!(
        out["events"],
        json!(["document.received", "email.received"])
    );

    // Same Message-ID, different bytes (re-sent by another server): caught.
    let again = email(
        "ap@quarry.example",
        Some("fixed@kingenv.com"),
        &[("b.pdf", &pdf())],
    );
    let e = commit(
        &db.state,
        &ap,
        "email.import",
        json!({ "raw_base64": b64(&again) }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&e, CommandError::Conflict(m) if m.contains("already here")),
        "{e}"
    );

    // Another organization sees none of it, and can import the same email.
    let other = payvy_server::Actor::agent(db.add_org().await, "their-agent");
    let theirs = commit(&db.state, &other, "email.list", json!({}))
        .await
        .unwrap();
    assert!(theirs["result"]["emails"].as_array().unwrap().is_empty());
    commit(
        &db.state,
        &other,
        "email.import",
        json!({ "raw_base64": b64(&raw) }),
    )
    .await
    .unwrap();
    db.drop().await;
}
