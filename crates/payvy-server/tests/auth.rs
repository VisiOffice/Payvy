//! Signing in to the web app with an emailed link, through the real router.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use payvy_server::auth::{Auth, CLIENT_HEADER};
use payvy_server::commands::CommandError;
use payvy_server::mailer::{Mail, Mailer};
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;
use common::{TestDb, agent, commit, person};

const APP: &str = "https://app.example.com";

async fn db() -> Option<(TestDb, Arc<Mutex<Vec<Mail>>>)> {
    let mut db = TestDb::new().await?;
    let sent = Arc::new(Mutex::new(Vec::new()));
    db.state = db
        .state
        .clone()
        .with_auth(Auth::new(APP, Mailer::Memory(sent.clone())));
    commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": "Robert@Example.com", "name": "Robert" }),
    )
    .await
    .unwrap();
    Some((db, sent))
}

struct Res {
    status: StatusCode,
    cookie: Option<String>,
    body: Value,
}

async fn call(db: &TestDb, req: Request<Body>) -> Res {
    let res = payvy_server::router(db.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Res {
        status,
        cookie,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn post(path: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn ask_for_link(db: &TestDb, email: &str) -> Res {
    call(db, post("/auth/link", json!({ "email": email }))).await
}

/// The mail is sent in the background; wait for it.
async fn mails(sent: &Arc<Mutex<Vec<Mail>>>, n: usize) -> Vec<Mail> {
    for _ in 0..100 {
        if sent.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    sent.lock().unwrap().clone()
}

fn token_in(mail: &Mail) -> String {
    let at = mail.text.find("#token=").expect("a link") + "#token=".len();
    mail.text[at..at + 64].to_owned()
}

/// `payvy_session=...` from a Set-Cookie header.
fn session(cookie: &str) -> String {
    cookie.split(';').next().unwrap().to_owned()
}

async fn sign_in(db: &TestDb, sent: &Arc<Mutex<Vec<Mail>>>) -> String {
    let before = sent.lock().unwrap().len();
    ask_for_link(db, "robert@example.com").await;
    let mail = mails(sent, before + 1).await.pop().unwrap();
    let res = call(
        db,
        post("/auth/session", json!({ "token": token_in(&mail) })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    session(&res.cookie.unwrap())
}

fn run_as(cookie: &str, client_header: bool) -> Request<Body> {
    let mut req = Request::post("/api/commands/person.list")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie);
    if client_header {
        req = req.header(CLIENT_HEADER, "web");
    }
    req.body(Body::from(json!({ "input": {} }).to_string()))
        .unwrap()
}

#[tokio::test]
async fn a_link_signs_in_once_and_the_cookie_runs_commands() {
    let Some((db, sent)) = db().await else { return };

    let res = ask_for_link(&db, " ROBERT@example.com ").await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    let mail = mails(&sent, 1).await.remove(0);
    assert_eq!(mail.to, "robert@example.com");
    assert!(
        mail.text.contains(&format!("{APP}/sign-in#token=")),
        "{}",
        mail.text
    );
    let token = token_in(&mail);

    let res = call(&db, post("/auth/session", json!({ "token": token }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["person"]["actor"], "robert");
    assert_eq!(res.body["organization"]["name"], "Default");
    let set = res.cookie.unwrap();
    assert!(
        set.contains("HttpOnly") && set.contains("Secure") && set.contains("SameSite=Lax"),
        "{set}"
    );
    let cookie = session(&set);

    // The same link again does nothing.
    let again = call(&db, post("/auth/session", json!({ "token": token }))).await;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED);

    let res = call(&db, run_as(&cookie, true)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        res.body["result"]["people"][0]["email"],
        "robert@example.com"
    );

    // Without the client header a cookie isn't enough: a form on another
    // site could send the cookie, but not the header.
    let res = call(&db, run_as(&cookie, false)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);

    let me = call(
        &db,
        Request::get("/auth/session")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(me.body["person"]["name"], "Robert");

    let out = call(
        &db,
        Request::delete("/auth/session")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert!(out.cookie.unwrap().contains("Max-Age=0"));
    let res = call(&db, run_as(&cookie, true)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    db.drop().await;
}

#[tokio::test]
async fn an_unknown_address_gets_the_same_answer_and_no_mail() {
    let Some((db, sent)) = db().await else { return };
    let known = ask_for_link(&db, "robert@example.com").await;
    let unknown = ask_for_link(&db, "mallory@example.com").await;
    assert_eq!(known.status, unknown.status);
    assert_eq!(known.body, unknown.body);
    let got = mails(&sent, 2).await;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].to, "robert@example.com");
    db.drop().await;
}

#[tokio::test]
async fn links_expire_and_are_limited() {
    let Some((db, sent)) = db().await else { return };
    ask_for_link(&db, "robert@example.com").await;
    let token = token_in(&mails(&sent, 1).await[0]);
    *db.clock.lock().unwrap() += chrono::Duration::minutes(16);
    let res = call(&db, post("/auth/session", json!({ "token": token }))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.body["error"]["code"], "link_expired");

    for _ in 0..7 {
        ask_for_link(&db, "robert@example.com").await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    // One expired link, then five in this window; the rest were not sent.
    assert_eq!(sent.lock().unwrap().len(), 6);
    db.drop().await;
}

#[tokio::test]
async fn removing_a_person_signs_them_out() {
    let Some((db, sent)) = db().await else { return };
    let cookie = sign_in(&db, &sent).await;
    commit(
        &db.state,
        &person("admin"),
        "person.remove",
        json!({ "actor": "robert" }),
    )
    .await
    .unwrap();
    let res = call(&db, run_as(&cookie, true)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    db.drop().await;
}

#[tokio::test]
async fn only_people_add_people_and_names_are_unique() {
    let Some((db, _)) = db().await else { return };
    let e = commit(
        &db.state,
        &agent("claude-ap"),
        "person.add",
        json!({ "email": "x@example.com", "name": "X" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");

    let e = commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": "robert@other.com", "name": "Another Robert" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");

    let ok = commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": "robert+ap@other.com", "name": "Another Robert", "actor": "robert.b" }),
    )
    .await
    .unwrap();
    assert_eq!(ok["result"]["person"]["actor"], "robert.b");
    assert_eq!(ok["events"][0], "person.added");
    db.drop().await;
}

#[tokio::test]
async fn one_email_in_two_companies_gets_a_link_for_each() {
    let Some((db, sent)) = db().await else { return };
    let other = db.add_org().await;
    commit(
        &db.state,
        &payvy_server::Actor::person(other, "admin"),
        "person.add",
        json!({ "email": "robert@example.com", "name": "Robert" }),
    )
    .await
    .unwrap();
    ask_for_link(&db, "robert@example.com").await;
    let mail = mails(&sent, 1).await.remove(0);
    assert_eq!(mail.text.matches("#token=").count(), 2, "{}", mail.text);
    assert!(mail.text.contains("Default:") && mail.text.contains("Other:"));
    db.drop().await;
}

#[tokio::test]
async fn without_a_public_url_signing_in_is_off() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let res = ask_for_link(&db, "robert@example.com").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.body["error"]["code"], "not_configured");
    db.drop().await;
}
