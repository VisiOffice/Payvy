//! Settings > Billing (hosted only): the billing.* commands ask the
//! management app, server to server, for a Checkout or portal link for the
//! signed-in company. The company comes from the session; the link grants
//! nothing and never enters the history.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::routing::{get, post};
use payvy_server::auth::Auth;
use payvy_server::billing::Billing;
use payvy_server::mailer::Mailer;
use serde_json::{Value, json};

mod common;
use common::{TestDb, agent, commit, entitle, hosted, person};

const APP: &str = "https://app.example.com";
const KEY: &str = "billing-key-0123456789abcdef0123456789";

/// Path, Authorization header and body of each request.
type Seen = Vec<(String, Option<String>, Value)>;

#[derive(Clone, Default)]
struct Fake {
    seen: Arc<Mutex<Seen>>,
    /// What checkout answers: a fresh Checkout, or the portal for a company
    /// that already subscribes.
    subscribed: Arc<Mutex<bool>>,
    refuse: Arc<Mutex<bool>>,
    /// Answer with a link that isn't https.
    plain_link: Arc<Mutex<bool>>,
}

async fn fake_billing(fake: Fake) -> String {
    async fn record(f: &Fake, uri: &Uri, h: &HeaderMap, body: Value) {
        let auth = h
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned());
        f.seen.lock().unwrap().push((uri.to_string(), auth, body));
    }
    let app = axum::Router::new()
        .route(
            "/api/billing/checkout",
            post(
                |State(f): State<Fake>, uri: Uri, h: HeaderMap, Json(b): Json<Value>| async move {
                    record(&f, &uri, &h, b).await;
                    if *f.refuse.lock().unwrap() {
                        return (
                            StatusCode::UNPROCESSABLE_ENTITY,
                            Json(json!({ "error": { "code": "invalid", "message": "plan isn't offered" } })),
                        );
                    }
                    let kind = if *f.subscribed.lock().unwrap() { "portal" } else { "checkout" };
                    let scheme = if *f.plain_link.lock().unwrap() { "http" } else { "https" };
                    (StatusCode::OK, Json(json!({ "kind": kind, "url": format!("{scheme}://pay.example/{kind}/1") })))
                },
            ),
        )
        .route(
            "/api/billing/portal",
            post(
                |State(f): State<Fake>, uri: Uri, h: HeaderMap, Json(b): Json<Value>| async move {
                    record(&f, &uri, &h, b).await;
                    Json(json!({ "kind": "portal", "url": "https://pay.example/portal/1" }))
                },
            ),
        )
        .route(
            "/api/billing/status",
            get(|State(f): State<Fake>, uri: Uri, h: HeaderMap| async move {
                record(&f, &uri, &h, Value::Null).await;
                Json(json!({
                    "subscription": {
                        "plan": "byoa", "status": "past_due", "trial_end": null,
                        "current_period_end": "2026-12-01T00:00:00.000Z",
                        "cancel_at_period_end": false,
                        "payments_stop_at": "2026-11-20T12:00:00.000Z",
                        "portal_url": "https://pay.example/portal/leak",
                    },
                    "offers": [{ "id": "byoa", "name": "Bring your own agent", "price": "$39 a month",
                                 "description": ["Your agent reads."], "trial_days": 14,
                                 "checkout_url": "https://pay.example/checkout/leak" }],
                    "customer_id": "cus_secret",
                }))
            }),
        )
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

async fn setup() -> Option<(TestDb, payvy_server::AppState, Fake)> {
    let db = TestDb::new().await?;
    let fake = Fake::default();
    let url = fake_billing(fake.clone()).await;
    let state = hosted(&db.state)
        .with_auth(Auth::new(APP, Mailer::Memory(Arc::default())))
        .with_billing(Billing::new(&url, KEY).unwrap());
    commit(
        &state,
        &person("admin"),
        "person.add",
        json!({ "email": "Robert@Example.com", "name": "Robert", "actor": "robert", "roles": ["admin"] }),
    )
    .await
    .unwrap();
    Some((db, state, fake))
}

async fn events(state: &payvy_server::AppState) -> Vec<Value> {
    let r = commit(
        state,
        &person("robert"),
        "event.list",
        json!({ "entity_id": payvy_store::DEFAULT_ORG }),
    )
    .await
    .unwrap();
    r["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"].as_str().unwrap().starts_with("billing."))
        .cloned()
        .collect()
}

#[tokio::test]
async fn checkout_sends_the_sessions_company_and_keeps_the_link_out_of_history() {
    let Some((_db, state, fake)) = setup().await else {
        return;
    };
    let r = commit(
        &state,
        &person("robert"),
        "billing.checkout",
        json!({ "plan": "managed" }),
    )
    .await
    .unwrap();
    assert_eq!(
        r["result"],
        json!({ "kind": "checkout", "url": "https://pay.example/checkout/1" })
    );
    assert_eq!(r["events"], json!(["billing.checkout_opened"]));

    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let (path, auth, body) = &seen[0];
    assert_eq!(path, "/api/billing/checkout");
    assert_eq!(auth.as_deref(), Some(format!("Bearer {KEY}").as_str()));
    assert_eq!(body["org_id"], json!(payvy_store::DEFAULT_ORG));
    assert_eq!(body["email"], "robert@example.com");
    assert_eq!(body["plan"], "managed");
    assert_eq!(body["return_url"], format!("{APP}/settings/billing"));

    let ev = events(&state).await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["actor"], "robert");
    assert!(!ev[0].to_string().contains("pay.example"), "{}", ev[0]);
}

#[tokio::test]
async fn a_subscribed_company_is_sent_to_the_portal() {
    let Some((_db, state, fake)) = setup().await else {
        return;
    };
    *fake.subscribed.lock().unwrap() = true;
    let r = commit(
        &state,
        &person("robert"),
        "billing.checkout",
        json!({ "plan": "byoa" }),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["kind"], "portal");
    assert_eq!(r["events"], json!(["billing.portal_opened"]));

    let r = commit(&state, &person("robert"), "billing.portal", json!({}))
        .await
        .unwrap();
    assert_eq!(r["result"]["url"], "https://pay.example/portal/1");
}

#[tokio::test]
async fn preview_asks_nothing_and_billing_get_shows_both_sides() {
    let Some((_db, state, fake)) = setup().await else {
        return;
    };
    let r = payvy_server::commands::run(
        &state,
        &person("robert"),
        "billing.checkout",
        json!({ "plan": "byoa" }),
        payvy_server::commands::Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["url"], Value::Null);
    assert!(fake.seen.lock().unwrap().is_empty());

    entitle(&state, payvy_store::DEFAULT_ORG, false, true, 1)
        .await
        .unwrap();
    let r = commit(&state, &person("robert"), "billing.get", json!({}))
        .await
        .unwrap();
    assert_eq!(r["result"]["plan"]["reader"], false);
    assert_eq!(r["result"]["plan"]["payments"], true);
    let b = &r["result"];
    assert_eq!(b["available"], true);
    assert_eq!(b["subscription"]["status"], "past_due");
    assert_eq!(
        b["subscription"]["payments_stop_at"],
        "2026-11-20T12:00:00Z"
    );
    assert_eq!(b["offers"][0]["price"], "$39 a month");
    assert_eq!(b["offers"][0]["trial_days"], 14);
    // Only the known fields pass through: nothing the management app adds
    // (a link, a Stripe id) reaches an agent or the page.
    let text = b.to_string();
    for leak in ["leak", "cus_secret", "portal_url", "checkout_url"] {
        assert!(!text.contains(leak), "{leak} passed through: {text}");
    }
}

#[tokio::test]
async fn agents_self_hosted_and_unconfigured_installs_get_no_billing() {
    let Some((db, state, _fake)) = setup().await else {
        return;
    };
    let e = commit(
        &state,
        &agent("claude"),
        "billing.checkout",
        json!({ "plan": "byoa" }),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), "person_only");
    let e = commit(&state, &agent("claude"), "billing.portal", json!({}))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "person_only");
    // The status is a shared read: an agent can see why a read was refused.
    let r = commit(&state, &agent("claude"), "billing.get", json!({}))
        .await
        .unwrap();
    assert_eq!(r["result"]["subscription"]["plan"], "byoa");

    // Self-hosted: no plans, whatever is configured.
    let e = commit(&db.state, &person("robert"), "billing.get", json!({}))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "not_configured");
    // Hosted without the billing settings.
    let e = commit(
        &hosted(&db.state),
        &person("robert"),
        "billing.portal",
        json!({}),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), "not_configured");
    // Only the two plans.
    let e = commit(
        &state,
        &person("robert"),
        "billing.checkout",
        json!({ "plan": "free" }),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), "invalid_input");
}

#[tokio::test]
async fn a_refusal_or_an_outage_is_a_billing_error_with_nothing_recorded() {
    let Some((db, state, fake)) = setup().await else {
        return;
    };
    *fake.refuse.lock().unwrap() = true;
    let e = commit(
        &state,
        &person("robert"),
        "billing.checkout",
        json!({ "plan": "byoa" }),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), "billing_error");
    assert!(e.to_string().contains("plan isn't offered"), "{e}");

    // Nothing listening there.
    let down = hosted(&db.state)
        .with_auth(Auth::new(APP, Mailer::Memory(Arc::default())))
        .with_billing(Billing::new("http://127.0.0.1:9", KEY).unwrap());
    let e = commit(&down, &person("robert"), "billing.portal", json!({}))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "billing_error");
    // The page still shows the plan in force when billing is down.
    let r = commit(&down, &person("robert"), "billing.get", json!({}))
        .await
        .unwrap();
    assert_eq!(r["result"]["available"], false);
    assert_eq!(r["result"]["plan"]["hosted"], true);
    assert_eq!(r["result"]["offers"], json!([]));

    // A link that isn't https never reaches the browser.
    *fake.refuse.lock().unwrap() = false;
    *fake.plain_link.lock().unwrap() = true;
    let e = commit(
        &state,
        &person("robert"),
        "billing.checkout",
        json!({ "plan": "byoa" }),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), "billing_error");
    assert!(events(&state).await.is_empty());
}
