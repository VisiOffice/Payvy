use super::*;
use axum::{Router, body::Body, extract::State, http::Request, response::Response};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

const ID: &str = "00000000-0000-4000-8000-000000000001";
const SECOND_ID: &str = "00000000-0000-4000-8000-000000000002";

#[derive(Clone)]
struct Reply {
    status: u16,
    body: Value,
    headers: Vec<(&'static str, String)>,
}

impl Reply {
    fn ok(body: Value) -> Self {
        Self {
            status: 200,
            body,
            headers: vec![],
        }
    }
}

struct Captured {
    method: String,
    uri: String,
    authorization: String,
    body: String,
}

#[derive(Clone)]
struct FakeState {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<Captured>>>,
}

struct Fake {
    origin: Url,
    state: FakeState,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fake {
    async fn start(replies: Vec<Reply>) -> Self {
        let state = FakeState {
            replies: Arc::new(Mutex::new(replies.into())),
            requests: Arc::new(Mutex::new(vec![])),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let app = Router::new().fallback(respond).with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            origin,
            state,
            task,
        }
    }

    fn client(&self) -> Ramp {
        Ramp::at(self.origin.clone(), "test-access-token").unwrap()
    }

    fn oauth(&self) -> OAuth {
        let mut client = OAuth::new(Environment::Sandbox, "test-client", "test-secret").unwrap();
        client.origin = self.origin.clone();
        client
    }

    fn count(&self) -> usize {
        self.state.requests.lock().unwrap().len()
    }
}

async fn respond(State(state): State<FakeState>, request: Request<Body>) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES).await.unwrap();
    state.requests.lock().unwrap().push(Captured {
        method: parts.method.to_string(),
        uri: parts.uri.to_string(),
        authorization: parts
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned(),
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    });
    let reply = state.replies.lock().unwrap().pop_front().unwrap_or(Reply {
        status: 500,
        body: json!({}),
        headers: vec![],
    });
    let mut response = Response::builder()
        .status(reply.status)
        .header("content-type", "application/json");
    for (name, value) in reply.headers {
        response = response.header(name, value);
    }
    response.body(Body::from(reply.body.to_string())).unwrap()
}

fn page(rows: Value, next: Value) -> Reply {
    Reply::ok(json!({"data": rows, "page": {"next": next}}))
}
fn vendor(id: &str) -> Value {
    json!({"id":id,"name":"Supply Co","is_active":true})
}
fn payment(status: &str, detail: Value) -> Value {
    json!({"id":ID,"entity_id":ID,"source_bank_account_id":ID,"vendor_id":ID,
        "payment_method":{"type":"ACH","id":ID},"amount":{"value":12345,"currency":"USD"},
        "external_ref":"payvy-attempt-1","scheduled_date":"2026-11-10",
        "status":status,"status_detail":detail})
}
fn tokens() -> Value {
    json!({"access_token":"new-access-token","expires_in":3600,"scope":DISCOVERY_SCOPES,"token_type":"Bearer"})
}

#[test]
fn production_and_sandbox_origins_are_fixed() {
    assert_eq!(
        Ramp::new(Environment::Production, "token")
            .unwrap()
            .origin
            .as_str(),
        "https://api.ramp.com/"
    );
    assert_eq!(
        Ramp::new(Environment::Sandbox, "token")
            .unwrap()
            .origin
            .as_str(),
        "https://demo-api.ramp.com/"
    );
    assert!(matches!(
        Ramp::new(Environment::Sandbox, "\n"),
        Err(Error::InvalidInput)
    ));
}

#[tokio::test]
async fn discovery_reads_allowlisted_fields_without_fabricating_balances() {
    let fake = Fake::start(vec![
        Reply::ok(json!({"id":ID,"business_name_legal":"Test Company","active":true,"phone":"secret"})),
        page(json!([{"id":ID,"entity_name":"US","currency":"USD","payment_accounts":[{
            "id":ID,"account_name":"Operating","account_number_last_four":"0042","currency":"USD"
        }]}]), Value::Null),
        page(json!([{"id":ID,"account_name":"Operating","account_number_last_four":"0042",
            "financial_institution_name":null,"has_ach_details":true,
            "account_number":"000123450042","routing_number":"121000248"}]), Value::Null),
    ]).await;
    let client = fake.client();
    assert_eq!(
        client.business().await.unwrap().business_name_legal,
        "Test Company"
    );
    assert_eq!(
        client.entities().await.unwrap()[0].payment_accounts[0].id,
        Uuid::parse_str(ID).unwrap()
    );
    let accounts = client.accounts().await.unwrap();
    assert_eq!(accounts[0].account_number_last_four.masked(), "…0042");
    let rendered = serde_json::to_string(&accounts).unwrap();
    assert!(!rendered.contains("000123450042") && !rendered.contains("121000248"));
    assert!(!rendered.contains("balance") && !rendered.contains("available_cents"));
    assert!(
        fake.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.method == "GET" && r.authorization == "Bearer test-access-token")
    );
}

#[tokio::test]
async fn vendors_follow_cursor_pages() {
    let fake = Fake::start(vec![
        page(
            json!([vendor(ID)]),
            json!("/developer/v1/vendors?start=next&page_size=100"),
        ),
        page(json!([vendor(SECOND_ID)]), Value::Null),
    ])
    .await;
    let vendors = fake.client().vendors().await.unwrap();
    assert_eq!(vendors.len(), 2);
    assert_eq!(vendors[1].id, Uuid::parse_str(SECOND_ID).unwrap());
    assert!(
        fake.state.requests.lock().unwrap()[1]
            .uri
            .contains("start=next")
    );
}

#[tokio::test]
async fn pagination_rejects_foreign_origins_credentials_fragments_and_paths() {
    for next in [
        "https://attacker.invalid/developer/v1/vendors?start=x",
        "//attacker.invalid/developer/v1/vendors?start=x",
        "/developer/v1/payments?start=x",
        "/developer/v1/vendors?start=x#fragment",
    ] {
        let fake = Fake::start(vec![page(json!([vendor(ID)]), json!(next))]).await;
        assert!(matches!(
            fake.client().vendors().await,
            Err(Error::InvalidResponse)
        ));
        assert_eq!(fake.count(), 1);
    }
    let fake = Fake::start(vec![]).await;
    let mut next = fake.origin.join("/developer/v1/vendors").unwrap();
    next.set_username("attacker").unwrap();
    fake.state
        .replies
        .lock()
        .unwrap()
        .push_back(page(json!([]), json!(next.as_str())));
    assert!(matches!(
        fake.client().vendors().await,
        Err(Error::InvalidResponse)
    ));
    assert_eq!(fake.count(), 1);
}

#[tokio::test]
async fn repeated_pages_and_missing_page_markers_are_errors_not_partial_success() {
    let fake = Fake::start(vec![page(
        json!([vendor(ID)]),
        json!("/developer/v1/vendors?page_size=100"),
    )])
    .await;
    assert!(matches!(
        fake.client().vendors().await,
        Err(Error::InvalidResponse)
    ));
    assert_eq!(fake.count(), 1);
    for body in [
        json!({"data":[]}),
        json!({"data":[],"page":{}}),
        json!({"data":[],"page":{"next":true}}),
    ] {
        let fake = Fake::start(vec![Reply::ok(body)]).await;
        assert!(matches!(
            fake.client().vendors().await,
            Err(Error::InvalidResponse)
        ));
    }
}

#[tokio::test]
async fn pagination_limit_does_not_return_a_truncated_inventory() {
    let replies = (0..MAX_PAGES)
        .map(|i| page(json!([]), json!(format!("/developer/v1/vendors?start={i}"))))
        .collect();
    let fake = Fake::start(replies).await;
    assert!(matches!(
        fake.client().vendors().await,
        Err(Error::InvalidResponse)
    ));
    assert_eq!(fake.count(), MAX_PAGES);
}

#[tokio::test]
async fn redirects_never_receive_credentials() {
    let destination = Fake::start(vec![Reply::ok(json!({}))]).await;
    let fake = Fake::start(vec![Reply {
        status: 302,
        body: json!({}),
        headers: vec![("location", destination.origin.to_string())],
    }])
    .await;
    assert!(matches!(
        fake.client().business().await,
        Err(Error::Rejected { status: 302 })
    ));
    assert_eq!(destination.count(), 0);
}

#[tokio::test]
async fn error_bodies_never_escape_and_rate_limits_preserve_retry_delay() {
    for (status, expected) in [
        (401, Error::Unauthorized),
        (403, Error::Unauthorized),
        (400, Error::Rejected { status: 400 }),
        (503, Error::Unavailable),
        (
            429,
            Error::RateLimited {
                retry_after_seconds: Some(60),
            },
        ),
    ] {
        let fake = Fake::start(vec![Reply {
            status,
            body: json!({"message":"token secret / routing 121000248"}),
            headers: vec![("retry-after", "60".into())],
        }])
        .await;
        let error = fake.client().business().await.unwrap_err();
        assert_eq!(error, expected);
        assert!(
            !error.to_string().contains("secret") && !format!("{error:?}").contains("121000248")
        );
        assert_eq!(fake.count(), 1, "the client must not retry implicitly");
    }
}

#[tokio::test]
async fn only_ach_instruments_and_safe_last_four_are_exposed() {
    let ach = json!({"id":ID,"type":"ACH","account_number_last_four":"0042",
        "fee":{"amount":100,"currency_code":"USD","minor_unit_conversion_rate":100},
        "account_number":"000123450042","routing_number":"121000248"});
    let fake = Fake::start(vec![page(
        json!([ach.clone(),{"type":"CHECK","address":"private"},{"type":"FUTURE_METHOD"}]),
        Value::Null,
    )])
    .await;
    let methods = fake
        .client()
        .ach_instruments(Uuid::parse_str(ID).unwrap())
        .await
        .unwrap();
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].fee.amount, 100);
    assert!(
        !serde_json::to_string(&methods)
            .unwrap()
            .contains("000123450042")
    );
    let mut bad = ach;
    bad["account_number_last_four"] = json!("000123450042");
    let fake = Fake::start(vec![page(json!([bad]), Value::Null)]).await;
    assert!(matches!(
        fake.client()
            .ach_instruments(Uuid::parse_str(ID).unwrap())
            .await,
        Err(Error::InvalidResponse)
    ));
}

#[tokio::test]
async fn recovery_includes_cancelled_and_refuses_changed_filters() {
    let fake = Fake::start(vec![page(
        json!([payment("CANCELED", json!("CANCELED"))]),
        Value::Null,
    )])
    .await;
    let payments = fake
        .client()
        .payments_by_reference("payvy-attempt-1")
        .await
        .unwrap();
    assert_eq!(payments[0].assessment(), Assessment::Cancelled);
    let request_uri = fake.state.requests.lock().unwrap()[0].uri.clone();
    assert!(
        request_uri.contains("external_ref=payvy-attempt-1")
            && request_uri.contains("include_canceled=true")
    );
    for next in [
        "/developer/v1/payments?start=next",
        "/developer/v1/payments?external_ref=other&include_canceled=true",
    ] {
        let fake = Fake::start(vec![page(json!([]), json!(next))]).await;
        assert!(matches!(
            fake.client().payments_by_reference("payvy-attempt-1").await,
            Err(Error::InvalidResponse)
        ));
        assert_eq!(fake.count(), 1);
    }
}

#[tokio::test]
async fn payment_reads_reject_wrong_ids_and_references() {
    let fake = Fake::start(vec![Reply::ok(payment("COMPLETED", json!("DELIVERED")))]).await;
    assert!(matches!(
        fake.client()
            .payment(Uuid::parse_str(SECOND_ID).unwrap())
            .await,
        Err(Error::InvalidResponse)
    ));
    let fake = Fake::start(vec![page(
        json!([payment("SCHEDULED", json!("SCHEDULED"))]),
        Value::Null,
    )])
    .await;
    assert!(matches!(
        fake.client().payments_by_reference("other-attempt").await,
        Err(Error::InvalidResponse)
    ));
}

#[test]
fn ambiguous_statuses_cannot_authorize_repayment() {
    for (status, detail, expected) in [
        ("SCHEDULED", json!("SCHEDULED"), Assessment::Pending),
        ("PROCESSING", json!("DEBITING"), Assessment::Pending),
        ("COMPLETED", json!("DELIVERED"), Assessment::Completed),
        ("CANCELED", json!("CANCELED"), Assessment::Cancelled),
        ("ERROR", json!("FAILED"), Assessment::NeedsReview),
        ("COMPLETED", json!("FAILED"), Assessment::NeedsReview),
        ("SCHEDULED", json!("DELIVERED"), Assessment::NeedsReview),
        ("COMPLETED", Value::Null, Assessment::NeedsReview),
        (
            "FUTURE_STATE",
            json!("FUTURE_DETAIL"),
            Assessment::NeedsReview,
        ),
    ] {
        let value: Payment = serde_json::from_value(payment(status, detail)).unwrap();
        assert_eq!(value.assessment(), expected);
    }
    let mut value: Payment =
        serde_json::from_value(payment("COMPLETED", json!("DELIVERED"))).unwrap();
    value.amount.currency = "EUR".into();
    assert_eq!(value.assessment(), Assessment::NeedsReview);
    value.amount.currency = "USD".into();
    value.amount.value = 0;
    assert_eq!(value.assessment(), Assessment::NeedsReview);
}

#[tokio::test]
async fn client_credentials_request_only_discovery_scopes() {
    let fake = Fake::start(vec![Reply::ok(tokens())]).await;
    let response = fake.oauth().client_credentials().await.unwrap();
    assert_eq!(response.expires_in, 3600);
    assert!(response.refresh_token.is_none());
    let requests = fake.state.requests.lock().unwrap();
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].uri, "/developer/v1/token");
    assert!(requests[0].authorization.starts_with("Basic "));
    assert!(requests[0].body.contains("grant_type=client_credentials"));
    assert!(!requests[0].body.contains("write") && !requests[0].body.contains("test-secret"));
}

#[tokio::test]
async fn refresh_keeps_existing_refresh_token_without_restarting_its_lifetime() {
    let fake = Fake::start(vec![Reply::ok(tokens())]).await;
    let response = fake
        .oauth()
        .refresh("original-refresh-token")
        .await
        .unwrap();
    assert_eq!(
        response.refresh_token.as_deref(),
        Some("original-refresh-token")
    );
    assert_eq!(response.refresh_token_expires_in, None);
    assert!(
        fake.state.requests.lock().unwrap()[0]
            .body
            .contains("grant_type=refresh_token")
    );
}

#[tokio::test]
async fn malformed_and_oversized_token_responses_fail_without_secrets() {
    for body in [
        json!({"access_token":"private-token"}),
        {
            let mut body = tokens();
            body["expires_in"] = json!(0);
            body
        },
        {
            let mut body = tokens();
            body["token_type"] = json!("unexpected");
            body
        },
        json!({"padding":"x".repeat(MAX_BODY_BYTES)}),
    ] {
        let fake = Fake::start(vec![Reply::ok(body)]).await;
        assert!(matches!(
            fake.oauth().client_credentials().await,
            Err(Error::InvalidResponse)
        ));
    }
}

#[tokio::test]
async fn authorization_uses_encoded_state_and_exact_redirect_and_only_read_scopes() {
    let fake = Fake::start(vec![Reply::ok(tokens())]).await;
    let oauth = fake.oauth();
    let redirect = "https://payvy.test/settings/bank";
    let url = Url::parse(
        &oauth
            .authorize_url(Environment::Sandbox, redirect, "state+/&?")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(url.host_str(), Some("demo.ramp.com"));
    let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(params["state"], "state+/&?");
    assert_eq!(params["redirect_uri"], redirect);
    assert_eq!(params["scope"], DISCOVERY_SCOPES);
    assert!(!params["scope"].contains(":write"));
    oauth
        .authorization_code("private-code+/&?", redirect)
        .await
        .unwrap();
    let requests = fake.state.requests.lock().unwrap();
    let form = Url::parse(&format!("http://unused/?{}", requests[0].body)).unwrap();
    let params: std::collections::HashMap<_, _> = form.query_pairs().collect();
    assert_eq!(params["grant_type"], "authorization_code");
    assert_eq!(params["code"], "private-code+/&?");
    assert_eq!(params["redirect_uri"], redirect);
}

#[test]
fn authorization_refuses_unsafe_redirects_and_fake_origin_overrides() {
    let oauth = OAuth::new(Environment::Sandbox, "client", "secret").unwrap();
    for redirect in [
        "http://public.test/settings/bank",
        "https://name:password@payvy.test/callback",
        "https://payvy.test/callback#fragment",
        "https://payvy.test/callback?code=old",
        "javascript:alert(1)",
    ] {
        assert!(
            oauth
                .authorize_url(Environment::Sandbox, redirect, "state")
                .is_err()
        );
    }
    for origin in [
        "https://api.ramp.com",
        "http://localhost:9999",
        "http://127.0.0.1@evil.test",
        "http://127.0.0.1/path",
        "http://127.0.0.1/?target=evil",
    ] {
        assert!(Ramp::sandbox_at(origin, "token").is_err());
        assert!(OAuth::sandbox_at(origin, "client", "secret").is_err());
    }
}

#[tokio::test]
async fn single_vendor_lookup_checks_identity_and_does_not_list_the_directory() {
    let fake = Fake::start(vec![Reply::ok(vendor(ID)), Reply::ok(vendor(SECOND_ID))]).await;
    let client = fake.client();
    assert_eq!(
        client
            .vendor(Uuid::parse_str(ID).unwrap())
            .await
            .unwrap()
            .name,
        "Supply Co"
    );
    assert!(matches!(
        client.vendor(Uuid::parse_str(ID).unwrap()).await,
        Err(Error::InvalidResponse)
    ));
    let requests = fake.state.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .all(|r| r.uri == format!("/developer/v1/vendors/{ID}"))
    );
}

#[tokio::test]
async fn vendor_search_encodes_filters_and_refuses_pagination_that_drops_them() {
    let fake = Fake::start(vec![page(
        json!([vendor(ID)]),
        json!("/developer/v1/vendors?start=next"),
    )])
    .await;
    let error = fake
        .client()
        .search_vendors("A&B / Supply")
        .await
        .unwrap_err();
    assert_eq!(error, Error::InvalidResponse);
    assert_eq!(fake.count(), 1);
    let request = &fake.state.requests.lock().unwrap()[0];
    let url = Url::parse(&format!("http://unused{}", request.uri)).unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(pairs["name"], "A&B / Supply");
    assert_eq!(pairs["is_active"], "true");
}
