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
    // Someone else still signs in: the last person can't be removed.
    commit(
        &db.state,
        &person("admin"),
        "person.add",
        json!({ "email": "dana@example.com", "name": "Dana" }),
    )
    .await
    .unwrap();
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

/// Robert in a second company, "Other", with the same address.
async fn in_two_companies(db: &TestDb) -> uuid::Uuid {
    let other = db.add_org().await;
    commit(
        &db.state,
        &payvy_server::Actor::administrator(other, "admin"),
        "person.add",
        json!({ "email": "robert@example.com", "name": "Robert" }),
    )
    .await
    .unwrap();
    other
}

/// Opens a fresh link for robert@example.com and returns the answer.
async fn open_link(db: &TestDb, sent: &Arc<Mutex<Vec<Mail>>>) -> Res {
    let before = sent.lock().unwrap().len();
    ask_for_link(db, "robert@example.com").await;
    let mail = mails(sent, before + 1).await.pop().unwrap();
    call(
        db,
        post("/auth/session", json!({ "token": token_in(&mail) })),
    )
    .await
}

fn with_cookie(
    req: axum::http::request::Builder,
    cookie: &str,
    client: bool,
) -> axum::http::request::Builder {
    let req = req.header(header::COOKIE, cookie);
    if client {
        req.header(CLIENT_HEADER, "web")
    } else {
        req
    }
}

fn switch(cookie: &str, org: uuid::Uuid, client: bool) -> Request<Body> {
    with_cookie(Request::post("/auth/session/switch"), cookie, client)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "org_id": org }).to_string()))
        .unwrap()
}

fn whoami(cookie: &str) -> Request<Body> {
    with_cookie(Request::get("/auth/session"), cookie, true)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn one_email_in_two_companies_gets_one_link_and_then_chooses() {
    let Some((db, sent)) = db().await else { return };
    let other = in_two_companies(&db).await;

    // One link, naming no company.
    ask_for_link(&db, "robert@example.com").await;
    let mail = mails(&sent, 1).await.remove(0);
    assert_eq!(mail.text.matches("#token=").count(), 1, "{}", mail.text);
    assert!(!mail.text.contains("Other"), "{}", mail.text);

    // Opening it proves the address, then lists only its companies.
    let res = call(
        &db,
        post("/auth/session", json!({ "token": token_in(&mail) })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.cookie.is_none(), "no session until a company is chosen");
    let names: Vec<&str> = res.body["choose"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Default", "Other"]);
    let ticket = res.body["ticket"].as_str().unwrap().to_owned();

    let res = call(
        &db,
        post(
            "/auth/session/choose",
            json!({ "ticket": ticket, "org_id": other }),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["organization"]["name"], "Other");
    assert_eq!(res.body["companies"].as_array().unwrap().len(), 2);
    assert!(res.cookie.is_some());

    // A ticket picks once.
    let res = call(
        &db,
        post(
            "/auth/session/choose",
            json!({ "ticket": ticket, "org_id": other }),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.body["error"]["code"], "link_expired");
    db.drop().await;
}

#[tokio::test]
async fn choosing_checks_membership_then_and_tickets_expire() {
    let Some((db, sent)) = db().await else { return };
    let other = in_two_companies(&db).await;
    let ticket = open_link(&db, &sent).await.body["ticket"]
        .as_str()
        .unwrap()
        .to_owned();

    // Removed from Other after proving the address: Other is refused, and
    // the same ticket still picks a company the address belongs to.
    commit(
        &db.state,
        &payvy_server::Actor::administrator(other, "admin"),
        "person.add",
        json!({ "email": "ops@example.com", "name": "Ops" }),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &payvy_server::Actor::administrator(other, "admin"),
        "person.remove",
        json!({ "actor": "robert" }),
    )
    .await
    .unwrap();
    let res = call(
        &db,
        post(
            "/auth/session/choose",
            json!({ "ticket": ticket, "org_id": other }),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(res.body["error"]["code"], "not_member");
    // A company the address never belonged to says the same.
    let res = call(
        &db,
        post(
            "/auth/session/choose",
            json!({ "ticket": ticket, "org_id": uuid::Uuid::new_v4() }),
        ),
    )
    .await;
    assert_eq!(res.body["error"]["code"], "not_member");
    let res = call(
        &db,
        post(
            "/auth/session/choose",
            json!({ "ticket": ticket, "org_id": payvy_store::DEFAULT_ORG }),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // Back in two companies; a ticket is good for ten minutes.
    commit(
        &db.state,
        &payvy_server::Actor::administrator(other, "admin"),
        "person.add",
        json!({ "email": "robert@example.com", "name": "Robert" }),
    )
    .await
    .unwrap();
    let ticket = open_link(&db, &sent).await.body["ticket"]
        .as_str()
        .unwrap()
        .to_owned();
    *db.clock.lock().unwrap() += chrono::Duration::minutes(11);
    let res = call(
        &db,
        post(
            "/auth/session/choose",
            json!({ "ticket": ticket, "org_id": other }),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    db.drop().await;
}

#[tokio::test]
async fn switching_company_rechecks_membership_and_ends_the_old_session() {
    let Some((db, sent)) = db().await else { return };
    // One company: the link signs straight in.
    let first = sign_in(&db, &sent).await;
    let other = in_two_companies(&db).await;

    // Not from a cross-site form.
    let res = call(&db, switch(&first, other, false)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    // Not into a company the address doesn't belong to.
    let res = call(&db, switch(&first, uuid::Uuid::new_v4(), true)).await;
    assert_eq!(res.body["error"]["code"], "not_member");

    let res = call(&db, switch(&first, other, true)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["organization"]["name"], "Other");
    let second = session(&res.cookie.unwrap());
    assert_eq!(
        call(&db, whoami(&first)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let me = call(&db, whoami(&second)).await;
    assert_eq!(me.body["organization"]["name"], "Other");

    // The session in Other runs commands in Other only.
    let res = call(&db, run_as(&second, true)).await;
    let people = res.body["result"]["people"].to_string();
    assert!(people.contains("robert@example.com"), "{people}");
    db.drop().await;
}

#[tokio::test]
async fn a_link_sent_for_a_new_company_opens_there() {
    let Some((mut db, sent)) = db().await else {
        return;
    };
    db.state = db.state.clone().with_operator_key(common::OPERATOR_KEY);
    let create = |key: &'static str| {
        let state = db.state.clone();
        async move {
            payvy_server::operator::run(
                &state,
                "org.create",
                json!({ "idempotency_key": key, "name": "Quarry Rentals",
                        "admin_email": "Robert@Example.com", "admin_name": "Robert",
                        "send_sign_in_link": true }),
                payvy_server::commands::Mode::Commit {
                    confirm_token: None,
                },
            )
            .await
            .unwrap()
        }
    };
    let made = create("signup-1").await;
    assert_eq!(made["result"]["sign_in_link"], "sent");
    let org = made["result"]["organization"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Robert is also in Default, but this link opens the new company.
    let mail = mails(&sent, 1).await.remove(0);
    let res = call(
        &db,
        post("/auth/session", json!({ "token": token_in(&mail) })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["organization"]["id"], org.as_str());
    assert_eq!(res.body["roles"], json!(["admin"]));

    // A replay of the signup sends nothing more.
    let again = create("signup-1").await;
    assert_eq!(again["result"]["sign_in_link"], "not_sent");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(sent.lock().unwrap().len(), 1);
    db.drop().await;
}

#[tokio::test]
async fn a_link_sent_before_links_proved_addresses_still_works() {
    let Some((db, _)) = db().await else { return };
    let robert = db
        .state
        .store
        .people_by_email("robert@example.com")
        .await
        .unwrap()
        .remove(0);
    let now = (db.state.clock)();
    let token = "a".repeat(64);
    db.state
        .store
        .insert_sign_in_link(
            &payvy_server::hash_key(&token),
            robert.person.id,
            now,
            now + chrono::Duration::minutes(15),
        )
        .await
        .unwrap();
    let res = call(&db, post("/auth/session", json!({ "token": token }))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = call(&db, post("/auth/session", json!({ "token": token }))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
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

#[tokio::test]
async fn bill_links_survive_sign_in_but_external_redirects_do_not() {
    let Some((db, sent)) = db().await else {
        return;
    };
    let bill = uuid::Uuid::new_v4();
    for (i, route) in [
        format!("/bills/{bill}"),
        "https://evil.example".into(),
        "//evil.example".into(),
        "/bills/not-a-uuid?next=https://evil.example".into(),
    ]
    .iter()
    .enumerate()
    {
        let response = call(
            &db,
            post(
                "/auth/link",
                json!({"email":"robert@example.com","return_to":route}),
            ),
        )
        .await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let mail = mails(&sent, i + 1).await.pop().unwrap();
        if i == 0 {
            assert!(
                mail.text
                    .contains(&format!("/sign-in?return_to=/bills/{bill}#token="))
            );
        } else {
            assert!(mail.text.contains("/sign-in#token="));
            assert!(!mail.text.contains("evil.example"));
        }
    }
    db.drop().await;
}

/// `org.create` as the management app's signup calls it.
async fn create_company(db: &TestDb, input: Value) -> Result<Value, CommandError> {
    payvy_server::operator::run(
        &db.state,
        "org.create",
        input,
        payvy_server::commands::Mode::Commit {
            confirm_token: None,
        },
    )
    .await
}

#[tokio::test]
async fn a_new_company_gets_the_welcome_and_asking_for_a_link_still_gets_the_sign_in_mail() {
    let Some((mut db, sent)) = db().await else {
        return;
    };
    db.state = db.state.clone().with_operator_key(common::OPERATOR_KEY);
    let company = "<script>alert(1)</script> & Sons";
    let note =
        "Your 14-day trial has started, no card needed.\nSee <b>https://evil.example</b> & more.";
    let signup = |note: &'static str| {
        json!({ "idempotency_key": "signup-welcome", "name": company,
                "admin_email": "Dana@Example.com", "admin_name": "Dana",
                "send_sign_in_link": true, "welcome_note": note })
    };
    let made = create_company(&db, signup(note)).await.unwrap();
    assert_eq!(made["result"]["created"], true);
    assert_eq!(made["result"]["sign_in_link"], "sent");

    let welcome = mails(&sent, 1).await.remove(0);
    let link = format!("{APP}/sign-in#token={}", token_in(&welcome));
    assert_eq!(welcome.to, "dana@example.com");
    assert_eq!(
        welcome.subject,
        format!("Welcome to Payvy: {company} is ready")
    );
    // The text part is complete on its own. This server takes no bills by
    // email, so the first step offers only what works here.
    assert_eq!(
        welcome.text,
        format!(
            "{company} was created in Payvy for dana@example.com.\n\n\
             Open this link to sign in:\n\n\
             {link}\n\n\
             The link works once, for 15 minutes. If it has expired, ask for a new one at {APP}/sign-in. If you didn't ask for it, ignore this email: nobody can sign in without opening it.\n\n\
             What to do first:\n\n\
             1. Send a first bill. Upload an invoice in the Inbox.\n\
             2. Connect the bank that pays, under Settings > Bank. Payvy pays through Mercury today, from your own account: it never holds your money.\n\
             3. Add the people who approve, under Settings > People. Payvy doesn't email them: each asks for a sign-in link at {APP}/sign-in.\n\n\
             {note}\n\n\
             Payvy: {APP}\n"
        )
    );
    let html = welcome.html.as_deref().expect("an HTML part");
    assert_eq!(html.matches(&format!("href=\"{link}\"")).count(), 2);
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt; &amp; Sons was created in Payvy"));
    assert!(html.contains(
        "no card needed.<br>See &lt;b&gt;https://evil.example&lt;/b&gt; &amp; more.</p>"
    ));
    assert!(!html.contains("<script"));
    assert!(!html.contains("<b>"));
    assert!(!html.contains("href=\"https://evil.example"));

    // A replay sends nothing, whatever note it carries: the note is not part
    // of what makes two calls the same signup.
    let again = create_company(&db, signup("Another note.")).await.unwrap();
    assert_eq!(again["result"]["created"], false);
    assert_eq!(again["result"]["sign_in_link"], "not_sent");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(sent.lock().unwrap().len(), 1);

    // Asking on the sign-in page is the ordinary mail, with nothing added.
    assert_eq!(
        ask_for_link(&db, "dana@example.com").await.status,
        StatusCode::ACCEPTED
    );
    let sign_in = mails(&sent, 2).await.remove(1);
    let link = format!("{APP}/sign-in#token={}", token_in(&sign_in));
    assert_eq!(sign_in.subject, "Sign in to Payvy");
    assert_eq!(
        sign_in.text,
        format!(
            "Open this link to sign in to Payvy:\n\n{link}\n\nThe link works once, for 15 minutes. If you didn't ask for it, ignore this email: nobody can sign in without opening it.\n"
        )
    );
    let html = sign_in.html.as_deref().expect("an HTML part");
    assert_eq!(html.matches(&format!("href=\"{link}\"")).count(), 2);
    assert!(!html.contains("What to do first"));
    assert!(!html.contains("Sons"));
    assert!(!html.contains("trial"));
    db.drop().await;
}

#[tokio::test]
async fn the_welcome_note_is_optional_and_limited() {
    let Some((mut db, sent)) = db().await else {
        return;
    };
    db.state = db.state.clone().with_operator_key(common::OPERATOR_KEY);
    let signup = |key: &str, email: &str, note: Option<String>| {
        let mut input = json!({ "idempotency_key": key, "name": "Quarry Rentals",
                                "admin_email": email, "admin_name": "Dana",
                                "send_sign_in_link": true });
        if let Some(note) = note {
            input["welcome_note"] = json!(note);
        }
        input
    };

    // One character over is refused before anything is created or sent.
    let e = create_company(
        &db,
        signup("note-1", "dana@example.com", Some("é".repeat(501))),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Invalid(_)), "{e}");
    assert_eq!(e.code(), "invalid_input");
    assert_eq!(
        e.to_string(),
        "invalid input: welcome_note can be at most 500 characters"
    );
    // So is anything but text, and a preview checks the same.
    let e = create_company(
        &db,
        signup("note-1", "dana@example.com", Some("a\u{1b}[31mb".into())),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), "invalid_input");
    let e = payvy_server::operator::run(
        &db.state,
        "org.create",
        signup("note-1", "dana@example.com", Some("x".repeat(501))),
        payvy_server::commands::Mode::Preview,
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), "invalid_input");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(sent.lock().unwrap().is_empty());

    // Exactly the limit goes out whole (counted in characters, not bytes);
    // the refused calls created nothing, so the same key still creates.
    let longest = "é".repeat(500);
    let made = create_company(
        &db,
        signup("note-1", "dana@example.com", Some(longest.clone())),
    )
    .await
    .unwrap();
    assert_eq!(made["result"]["created"], true);
    let with = mails(&sent, 1).await.remove(0);
    assert!(
        with.text
            .contains(&format!("\n\n{longest}\n\nPayvy: {APP}\n"))
    );
    assert!(with.html.as_deref().unwrap().contains(&longest));

    // Absent (or blank), there is no paragraph: the steps run into the close.
    for (key, email, note) in [
        ("note-2", "erin@example.com", None),
        ("note-3", "fay@example.com", Some("  \n ".to_owned())),
    ] {
        let before = sent.lock().unwrap().len();
        create_company(&db, signup(key, email, note)).await.unwrap();
        let without = mails(&sent, before + 1).await.pop().unwrap();
        assert!(
            without
                .text
                .ends_with(&format!("at {APP}/sign-in.\n\nPayvy: {APP}\n")),
            "{}",
            without.text
        );
        assert_eq!(
            without.html.as_deref().unwrap().matches("<p ").count() + 1,
            with.html.as_deref().unwrap().matches("<p ").count()
        );
    }
    db.drop().await;
}

#[tokio::test]
async fn the_welcome_link_keeps_the_rules_of_any_sign_in_link() {
    let Some((mut db, sent)) = db().await else {
        return;
    };
    db.state = db.state.clone().with_operator_key(common::OPERATOR_KEY);
    let signup = |key: &str| {
        json!({ "idempotency_key": key, "name": format!("Company {key}"),
                "admin_email": "dana@example.com", "admin_name": "Dana",
                "send_sign_in_link": true })
    };

    // Once.
    create_company(&db, signup("a")).await.unwrap();
    let token = token_in(&mails(&sent, 1).await[0]);
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    let open = || call(&db, post("/auth/session", json!({ "token": token })));
    assert_eq!(open().await.status, StatusCode::OK);
    let second = open().await;
    assert_eq!(second.status, StatusCode::UNAUTHORIZED);
    assert_eq!(second.body["error"]["code"], "link_expired");

    // For 15 minutes.
    *db.clock.lock().unwrap() += chrono::Duration::minutes(16);
    create_company(&db, signup("b")).await.unwrap();
    let token = token_in(&mails(&sent, 2).await[1]);
    *db.clock.lock().unwrap() += chrono::Duration::minutes(16);
    let late = call(&db, post("/auth/session", json!({ "token": token }))).await;
    assert_eq!(late.status, StatusCode::UNAUTHORIZED);
    assert_eq!(late.body["error"]["code"], "link_expired");

    // And it counts toward the address's five per window like any other:
    // one welcome and four sign-in mails, then nothing, welcome or not.
    let made = create_company(&db, signup("c")).await.unwrap();
    assert_eq!(made["result"]["sign_in_link"], "sent");
    for _ in 0..6 {
        ask_for_link(&db, "dana@example.com").await;
    }
    let refused = create_company(&db, signup("d")).await.unwrap();
    assert_eq!(refused["result"]["created"], true);
    assert_eq!(refused["result"]["sign_in_link"], "not_sent");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let all = sent.lock().unwrap().clone();
    assert_eq!(all.len(), 2 + 5);
    let welcomes = all
        .iter()
        .filter(|m| m.subject.starts_with("Welcome to Payvy: "))
        .count();
    assert_eq!(welcomes, 3);
    db.drop().await;
}
