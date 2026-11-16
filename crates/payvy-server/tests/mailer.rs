//! Sending mail through Amazon SES, against a fake SES that checks the
//! request's signature the way AWS does: from the request it received; and
//! through Cloudflare Email Service, against a fake that keeps what it got.
//! Each backend sends text alone exactly as it always did, and text with
//! HTML as its API's two parts.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{NaiveDateTime, TimeZone, Utc};
use payvy_server::mailer::{Mail, Mailer};
use payvy_server::sigv4;
use serde_json::{Value, json};

const KEY_ID: &str = "AKIDEXAMPLE";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

#[derive(Clone, Default)]
struct Fake {
    sent: Arc<Mutex<Vec<Value>>>,
    refuse: Arc<Mutex<bool>>,
}

async fn send(State(f): State<Fake>, headers: HeaderMap, body: Bytes) -> (StatusCode, Json<Value>) {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    let at = Utc.from_utc_datetime(
        &NaiveDateTime::parse_from_str(&h("x-amz-date"), "%Y%m%dT%H%M%SZ").expect("x-amz-date"),
    );
    let [_, (_, expected)] = sigv4::sign(
        &sigv4::Credentials {
            access_key_id: KEY_ID,
            secret_access_key: SECRET,
            region: "us-west-2",
            service: "ses",
        },
        &sigv4::Request {
            method: "POST",
            host: &h("host"),
            path: "/v2/email/outbound-emails",
            headers: &[("content-type", &h("content-type"))],
            body: &body,
        },
        at,
    );
    if h("authorization") != expected {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "message": "The request signature we calculated does not match" })),
        );
    }
    if *f.refuse.lock().unwrap() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "message": "Email address is not verified." })),
        );
    }
    f.sent
        .lock()
        .unwrap()
        .push(serde_json::from_slice(&body).unwrap());
    (StatusCode::OK, Json(json!({ "MessageId": "fake-1" })))
}

async fn fake_ses() -> (Fake, Mailer) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/v2/email/outbound-emails", post(send))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let Mailer::Ses {
        region,
        access_key_id,
        secret_access_key,
        from,
        http,
        ..
    } = Mailer::ses("us-west-2", KEY_ID, SECRET, "Payvy <no-reply@payvy.com>").unwrap()
    else {
        unreachable!()
    };
    let mailer = Mailer::Ses {
        region,
        access_key_id,
        secret_access_key,
        from,
        endpoint: format!("http://{addr}"),
        http,
    };
    (fake, mailer)
}

fn mail() -> Mail {
    Mail {
        to: "robert@example.com".into(),
        subject: "Sign in to Payvy".into(),
        text: "Open this link: https://beta.payvy.com/sign-in#token=abc\n".into(),
        html: None,
    }
}

#[tokio::test]
async fn ses_gets_a_signed_message() {
    let (fake, mailer) = fake_ses().await;
    mailer.send(mail()).await.unwrap();
    let sent = fake.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["FromEmailAddress"], "Payvy <no-reply@payvy.com>");
    assert_eq!(
        sent[0]["Destination"]["ToAddresses"][0],
        "robert@example.com"
    );
    assert_eq!(
        sent[0]["Content"]["Simple"]["Subject"]["Data"],
        "Sign in to Payvy"
    );
    assert!(
        sent[0]["Content"]["Simple"]["Body"]["Text"]["Data"]
            .as_str()
            .unwrap()
            .contains("#token=abc")
    );
}

#[tokio::test]
async fn a_refusal_says_why() {
    let (fake, mailer) = fake_ses().await;
    *fake.refuse.lock().unwrap() = true;
    let e = mailer.send(mail()).await.unwrap_err().to_string();
    assert!(e.contains("not verified"), "{e}");
}

#[tokio::test]
async fn a_wrong_secret_is_refused() {
    let (_fake, mailer) = fake_ses().await;
    let Mailer::Ses {
        region,
        access_key_id,
        from,
        endpoint,
        http,
        ..
    } = mailer
    else {
        unreachable!()
    };
    let wrong = Mailer::Ses {
        region,
        access_key_id,
        secret_access_key: "not-the-secret".into(),
        from,
        endpoint,
        http,
    };
    let e = wrong.send(mail()).await.unwrap_err().to_string();
    assert!(e.contains("signature"), "{e}");
}

const LINK: &str = "https://beta.payvy.com/sign-in#token=0000000000000000000000000000000000000000000000000000000000000000";

/// A sign-in mail as the server writes it: both parts.
fn both_parts() -> Mail {
    let mail = payvy_server::emails::sign_in("robert@example.com", LINK, 15);
    assert!(mail.html.is_some());
    mail
}

#[tokio::test]
async fn ses_gets_text_alone_as_before_and_both_parts_when_there_is_html() {
    let (fake, mailer) = fake_ses().await;
    mailer.send(mail()).await.unwrap();
    let mail = both_parts();
    mailer.send(mail.clone()).await.unwrap();
    let sent = fake.sent.lock().unwrap().clone();

    // Without HTML the request is what it was before HTML mail existed.
    assert_eq!(
        sent[0],
        json!({
            "FromEmailAddress": "Payvy <no-reply@payvy.com>",
            "Destination": { "ToAddresses": ["robert@example.com"] },
            "Content": { "Simple": {
                "Subject": { "Data": "Sign in to Payvy", "Charset": "UTF-8" },
                "Body": { "Text": {
                    "Data": "Open this link: https://beta.payvy.com/sign-in#token=abc\n",
                    "Charset": "UTF-8",
                } },
            } },
        })
    );

    let body = &sent[1]["Content"]["Simple"]["Body"];
    assert_eq!(
        body,
        &json!({
            "Text": { "Data": mail.text, "Charset": "UTF-8" },
            "Html": { "Data": mail.html, "Charset": "UTF-8" },
        })
    );
    assert!(body["Text"]["Data"].as_str().unwrap().contains(LINK));
    assert!(body["Html"]["Data"].as_str().unwrap().contains(LINK));
}

type CloudflareSent = Arc<Mutex<Vec<(String, String, Value)>>>;

async fn cloudflare_send(
    State(sent): State<CloudflareSent>,
    axum::extract::Path(account): axum::extract::Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    sent.lock().unwrap().push((account, auth, body));
    Json(json!({ "success": true, "errors": [], "result": { "permanent_bounces": [] } }))
}

async fn fake_cloudflare() -> (CloudflareSent, Mailer) {
    let sent = CloudflareSent::default();
    let app = Router::new()
        .route(
            "/accounts/{account}/email/sending/send",
            post(cloudflare_send),
        )
        .with_state(sent.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let Mailer::Cloudflare {
        account_id,
        token,
        from,
        http,
        ..
    } = Mailer::cloudflare("acct-1", "cf-test-token", "Payvy <no-reply@payvy.com>").unwrap()
    else {
        unreachable!()
    };
    let mailer = Mailer::Cloudflare {
        account_id,
        token,
        from,
        api_base: format!("http://{addr}"),
        http,
    };
    (sent, mailer)
}

#[tokio::test]
async fn cloudflare_gets_text_alone_as_before_and_both_parts_when_there_is_html() {
    let (sent, mailer) = fake_cloudflare().await;
    mailer.send(mail()).await.unwrap();
    let mail = both_parts();
    mailer.send(mail.clone()).await.unwrap();
    let sent = sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].0, "acct-1");
    assert_eq!(sent[0].1, "Bearer cf-test-token");

    // Without HTML the request is what it was before HTML mail existed.
    assert_eq!(
        sent[0].2,
        json!({
            "from": "Payvy <no-reply@payvy.com>",
            "to": "robert@example.com",
            "subject": "Sign in to Payvy",
            "text": "Open this link: https://beta.payvy.com/sign-in#token=abc\n",
        })
    );
    assert_eq!(
        sent[1].2,
        json!({
            "from": "Payvy <no-reply@payvy.com>",
            "to": "robert@example.com",
            "subject": "Sign in to Payvy",
            "text": mail.text,
            "html": mail.html,
        })
    );
    assert!(sent[1].2["text"].as_str().unwrap().contains(LINK));
}

#[tokio::test]
async fn the_memory_backend_keeps_both_parts() {
    let kept = Arc::new(Mutex::new(Vec::new()));
    let mailer = Mailer::Memory(kept.clone());
    let mail = both_parts();
    mailer.send(mail.clone()).await.unwrap();
    Mailer::Log.send(mail.clone()).await.unwrap();
    assert_eq!(kept.lock().unwrap().as_slice(), &[mail]);
}
