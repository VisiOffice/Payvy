//! Sending mail through Amazon SES, against a fake SES that checks the
//! request's signature the way AWS does: from the request it received.

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
