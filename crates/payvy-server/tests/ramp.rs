//! Real command dispatch + Postgres RLS + local Ramp only. No bank credentials.
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::IntoResponse,
};
use payvy_pay::ramp::{DISCOVERY_SCOPES, Environment};
use payvy_server::{
    Bank,
    commands::{self, Mode},
    ramp::Config,
    secrets::Sealer,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
mod common;
use common::{TestDb, agent, bill, commit, confirm, person, set_workflow, vendor};
const ID: &str = "00000000-0000-4000-8000-000000000001";
const OTHER: &str = "00000000-0000-4000-8000-000000000002";
#[derive(Default)]
struct Data {
    tokens: usize,
    refreshes: usize,
    last_four: bool,
    funding_changed: bool,
    inactive: bool,
    fail_token: bool,
    pause: Option<(String, Arc<Gate>)>,
    writes: usize,
    token_status: Option<u16>,
    advance_on_token: Option<Arc<Mutex<chrono::DateTime<chrono::Utc>>>>,
    business_status: Option<u16>,
    retry_after: Option<u64>,
    malformed_token: bool,
    missing_refresh: bool,
    token_ttl: Option<u64>,
    directory_reads: usize,
    vendor_reads: usize,
    business_reads: usize,
    webhook_deletes: usize,
    fail_webhook_delete: bool,
    mercury_checks: usize,
}
#[derive(Default)]
struct Gate {
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
struct Fake {
    url: String,
    data: Arc<Mutex<Data>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fake {
    async fn start() -> Self {
        let data = Arc::new(Mutex::new(Data::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(respond).with_state(data.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, data, task }
    }
    fn bank(&self) -> Bank {
        let mut bank = Bank::new(Sealer::from_hex(&"11".repeat(32)).unwrap());
        bank.mercury_sandbox = format!("{}/mercury", self.url);
        bank.ramp = Some(Config {
            client_id: "test-client".into(),
            client_secret: "secret-client".into(),
            redirect_uri: "https://payvy.test/settings/bank".into(),
            environment: Environment::Sandbox,
            sandbox_url: Some(self.url.clone()),
        });
        bank
    }
}
async fn respond(
    State(data): State<Arc<Mutex<Data>>>,
    req: Request<Body>,
) -> axum::response::Response {
    let (parts, body) = req.into_parts();
    let raw = axum::body::to_bytes(body, 10000).await.unwrap();
    let gate = {
        let mut d = data.lock().unwrap();
        if d.pause
            .as_ref()
            .is_some_and(|(path, _)| path == parts.uri.path())
        {
            d.pause.take().map(|(_, gate)| gate)
        } else {
            None
        }
    };
    if let Some(gate) = gate {
        gate.entered.notify_one();
        gate.resume.notified().await;
    }
    let mut d = data.lock().unwrap();
    let path = parts.uri.path();
    if path.starts_with("/mercury/") {
        assert_eq!(
            parts.headers["authorization"],
            "Bearer legacy-mercury-token"
        );
        if path == "/mercury/webhooks/legacy-hook" {
            assert_eq!(parts.method, "DELETE");
            d.webhook_deletes += 1;
            return (
                if d.fail_webhook_delete {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::OK
                },
                Json(json!({})),
            )
                .into_response();
        }
        assert_eq!(
            path,
            "/mercury/account/legacy-account/transaction/legacy-transaction"
        );
        assert_eq!(parts.method, "GET");
        d.mercury_checks += 1;
        return (
            StatusCode::OK,
            Json(json!({"id":"legacy-transaction","status":"sent"})),
        )
            .into_response();
    }
    let page = |data: Value| json!({"data":data,"page":{"next":null}});
    let value = if path == "/developer/v1/token" {
        assert_eq!(parts.method, "POST");
        assert!(
            parts.headers["authorization"]
                .to_str()
                .unwrap()
                .starts_with("Basic ")
        );
        d.tokens += 1;
        if let Some(clock) = &d.advance_on_token {
            *clock.lock().unwrap() += chrono::Duration::seconds(30);
        }
        if let Some(status) = d.token_status {
            let mut response = (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({"error":"private-grant-error-secret"})),
            )
                .into_response();
            if let Some(seconds) = d.retry_after {
                response
                    .headers_mut()
                    .insert("retry-after", seconds.to_string().parse().unwrap());
            }
            return response;
        }
        if d.malformed_token {
            return (
                StatusCode::OK,
                Json(json!({"access_token":"secret-should-not-escape"})),
            )
                .into_response();
        }
        if d.fail_token {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"message":"secret-client MUST NOT LEAK"})),
            )
                .into_response();
        }
        let refresh = String::from_utf8_lossy(&raw).contains("grant_type=refresh_token");
        if refresh {
            d.refreshes += 1;
            assert!(String::from_utf8_lossy(&raw).contains("refresh_token=secret-refresh"));
        }
        let mut t = json!({"access_token":"secret-access","token_type":"Bearer","scope":DISCOVERY_SCOPES,"expires_in":d.token_ttl.unwrap_or(3600)});
        if !refresh && !d.missing_refresh {
            t["refresh_token"] = json!("secret-refresh");
            t["refresh_token_expires_in"] = json!(7200);
        }
        t
    } else {
        assert_eq!(parts.headers["authorization"], "Bearer secret-access");
        if parts.method != "GET" {
            d.writes += 1;
            return (StatusCode::METHOD_NOT_ALLOWED, Json(json!({}))).into_response();
        }
        match path {
            "/developer/v1/business" => {
                d.business_reads += 1;
                if let Some(status) = d.business_status {
                    return (
                        StatusCode::from_u16(status).unwrap(),
                        Json(json!({"private":"secret-business-error"})),
                    )
                        .into_response();
                }
                json!({"id":ID,"business_name_legal":"Example Business","active":true})
            }
            "/developer/v1/entities" => page(
                json!([{"id":ID,"entity_name":"US Operations","currency":"USD","payment_accounts":[{"id":ID,"account_name":"Operating","account_number_last_four":if d.funding_changed {"9999"} else {"0042"},"currency":"USD"}]}]),
            ),
            "/developer/v1/bank-accounts" => page(
                json!([{"id":ID,"account_name":"Operating","account_number_last_four":if d.funding_changed {"9999"} else {"0042"},"has_ach_details":true,"financial_institution_name":"Test Bank"}]),
            ),
            "/developer/v1/vendors" => {
                d.directory_reads += 1;
                page(json!([{"id":ID,"name":"Supply Co","is_active":!d.inactive}]))
            }
            p if p == format!("/developer/v1/vendors/{ID}") => {
                d.vendor_reads += 1;
                json!({"id":ID,"name":"Supply Co","is_active":!d.inactive})
            }
            p if p == format!("/developer/v1/vendors/{ID}/payment-methods") => page(json!([
                {"id":ID,"type":"ACH","account_number_last_four":if d.last_four {"9876"} else {"1234"},"fee":{"amount":0,"currency_code":"USD","minor_unit_conversion_rate":100}},
                {"id":OTHER,"type":"ACH","account_number_last_four":"5678","fee":{"amount":100,"currency_code":"USD","minor_unit_conversion_rate":100}}
            ])),
            _ => panic!("unexpected Ramp call: {path}"),
        }
    };
    (StatusCode::OK, Json(value)).into_response()
}
async fn begin(db: &TestDb) -> (String, String) {
    let r = commit(&db.state, &person("robert"), "bank.authorize", json!({}))
        .await
        .unwrap();
    let url = reqwest::Url::parse(r["result"]["authorize_url"].as_str().unwrap()).unwrap();
    assert_eq!(url.host_str(), Some("demo.ramp.com"));
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    (
        r["result"]["authorization_id"].as_str().unwrap().into(),
        state,
    )
}
async fn ready(db: &TestDb) -> Value {
    let (id, state) = begin(db).await;
    let r = commit(
        &db.state,
        &person("robert"),
        "bank.complete_authorization",
        json!({"state":state,"code":"test-code"}),
    )
    .await
    .unwrap();
    assert_eq!(r["result"]["status"], "ready");
    let result = commit(
        &db.state,
        &person("robert"),
        "bank.authorization",
        json!({"authorization_id":id}),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert!(!result.to_string().contains("secret-"));
    result
}
fn activation(stage: &Value) -> Value {
    let c = &stage["choices"][0];
    json!({"authorization_id":stage["authorization_id"],"entity_id":c["entity_id"],"account_id":c["account_id"],"fingerprint":c["fingerprint"],"replacing_connection_id":stage["replacing"].get("id")})
}
async fn connected(db: &TestDb) -> String {
    let stage = ready(db).await;
    confirm(
        &db.state,
        &person("robert"),
        "bank.activate",
        activation(&stage),
    )
    .await
    .unwrap()["result"]["connection"]["id"]
        .as_str()
        .unwrap()
        .into()
}

#[tokio::test]
async fn authorization_is_person_org_bound_single_use_expiring_and_secret_free() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    db.state = db.state.clone().with_bank(fake.bank());
    assert!(
        commit(&db.state, &agent("bot"), "bank.authorize", json!({}))
            .await
            .is_err()
    );
    let (id, state) = begin(&db).await;
    let input = json!({"state":state,"code":"test-code"});
    assert!(
        commit(
            &db.state,
            &person("other"),
            "bank.complete_authorization",
            input.clone()
        )
        .await
        .is_err()
    );
    let other_org = db.add_org().await;
    let other = payvy_server::Actor::administrator(other_org, "robert");
    assert!(
        commit(
            &db.state,
            &other,
            "bank.complete_authorization",
            input.clone()
        )
        .await
        .is_err()
    );
    commands::run(
        &db.state,
        &person("robert"),
        "bank.complete_authorization",
        input.clone(),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(fake.data.lock().unwrap().tokens, 0);
    let completed = commit(
        &db.state,
        &person("robert"),
        "bank.complete_authorization",
        input.clone(),
    )
    .await
    .unwrap();
    assert!(!completed.to_string().contains("secret-"));
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "bank.complete_authorization",
            input
        )
        .await
        .is_err()
    );
    let bank = commit(&db.state, &person("robert"), "bank.get", json!({}))
        .await
        .unwrap();
    assert!(bank["result"]["connection"].is_null());
    assert!(
        commit(
            &db.state,
            &other,
            "bank.authorization",
            json!({"authorization_id":id})
        )
        .await
        .is_err()
    );
    *db.clock.lock().unwrap() += chrono::Duration::minutes(11);
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "bank.authorization",
            json!({"authorization_id":id})
        )
        .await
        .is_err()
    );
    let (_, state) = begin(&db).await;
    fake.data.lock().unwrap().fail_token = true;
    let input = json!({"state":state,"code":"bad-code"});
    let failure = commit(
        &db.state,
        &person("robert"),
        "bank.complete_authorization",
        input.clone(),
    )
    .await
    .unwrap();
    assert_eq!(failure["result"]["status"], "failed");
    assert!(!failure.to_string().contains("secret-client"));
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "bank.complete_authorization",
            input
        )
        .await
        .is_err()
    );
    assert_eq!(fake.data.lock().unwrap().tokens, 2);
    db.drop().await;
}

#[tokio::test]
async fn activation_previews_replacement_rechecks_funding_and_blocks_sends() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    db.state = db.state.clone().with_bank(fake.bank());
    // Existing Mercury and read-only Rho remain until confirmed activation.
    let old = Uuid::new_v4();
    let rho = Uuid::new_v4();
    let mut tx = db
        .state
        .store
        .begin(payvy_store::DEFAULT_ORG)
        .await
        .unwrap();
    for (id, provider, mode) in [(old, "mercury", "request"), (rho, "rho", "read_only")] {
        payvy_store::insert_connection(
            &mut tx,
            payvy_store::DEFAULT_ORG,
            &payvy_store::NewConnection {
                id,
                provider,
                environment: "sandbox",
                mode,
                account_id: "old-account",
                account_name: "Old bank",
                token_sealed: &[1],
                created_at: (db.state.clock)(),
                created_by: "robert",
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let stage = ready(&db).await;
    let input = activation(&stage);
    assert_eq!(input["replacing_connection_id"], old.to_string());
    let mut missing = input.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("replacing_connection_id");
    assert!(
        commands::run(
            &db.state,
            &person("robert"),
            "bank.activate",
            missing,
            Mode::Preview
        )
        .await
        .is_err()
    );
    assert!(
        commit(&db.state, &person("robert"), "bank.activate", input.clone())
            .await
            .is_err()
    );
    assert!(
        commands::run(
            &db.state,
            &agent("bot"),
            "bank.activate",
            input.clone(),
            Mode::Preview
        )
        .await
        .is_err()
    );
    let p = commands::run(
        &db.state,
        &person("robert"),
        "bank.activate",
        input.clone(),
        Mode::Preview,
    )
    .await
    .unwrap();
    let current = commit(&db.state, &person("robert"), "bank.get", json!({}))
        .await
        .unwrap();
    assert_eq!(current["result"]["connection"]["id"], old.to_string());
    fake.data.lock().unwrap().funding_changed = true;
    assert!(
        commands::run(
            &db.state,
            &person("robert"),
            "bank.activate",
            input.clone(),
            Mode::Commit {
                confirm_token: Some(p["confirm_token"].as_str().unwrap().into())
            }
        )
        .await
        .is_err()
    );
    fake.data.lock().unwrap().funding_changed = false;
    let mut wrong = input.clone();
    wrong["replacing_connection_id"] = Value::Null;
    assert!(
        commands::run(
            &db.state,
            &person("robert"),
            "bank.activate",
            wrong,
            Mode::Preview
        )
        .await
        .is_err()
    );
    let activated = confirm(&db.state, &person("robert"), "bank.activate", input.clone())
        .await
        .unwrap();
    assert_eq!(activated["result"]["connection"]["mode"], "setup");
    assert!(!activated.to_string().contains("secret-"));
    let other_org = db.add_org().await;
    let mut other_tx = db.state.store.begin(other_org).await.unwrap();
    let cid = Uuid::parse_str(activated["result"]["connection"]["id"].as_str().unwrap()).unwrap();
    assert!(
        payvy_store::ramp::details(&mut other_tx, cid)
            .await
            .unwrap()
            .is_none()
    );
    other_tx.rollback().await.unwrap();
    assert!(
        confirm(&db.state, &person("robert"), "bank.activate", input)
            .await
            .is_err()
    );
    let current = commit(&db.state, &person("robert"), "bank.get", json!({}))
        .await
        .unwrap();
    assert_eq!(
        current["result"]["connections"].as_array().unwrap().len(),
        2
    );
    let v = vendor(&db.state, &agent("bot"), "Supply Co").await;
    set_workflow(
        &db.state,
        &agent("bot"),
        None,
        json!([{"id":"pay","type":"pay","release":"manual"}]),
    )
    .await;
    let b = bill(&db.state, &agent("bot"), &v, "RAMP-1", 1000).await;
    let error = confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({"bill_id":b}),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("Ramp payments are not enabled"),
        "{error}"
    );
    confirm(
        &db.state,
        &person("robert"),
        "bill.release",
        json!({"bill_id":b,"pay_elsewhere":true}),
    )
    .await
    .unwrap();
    assert_eq!(fake.data.lock().unwrap().writes, 0);
    db.drop().await;
}

#[tokio::test]
async fn payees_pin_specific_instrument_and_acceptance_checks_changes_and_connection() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    db.state = db.state.clone().with_bank(fake.bank());
    let connection = connected(&db).await;
    let v = vendor(&db.state, &agent("bot"), "Local supplier").await;
    let list = commit(
        &db.state,
        &agent("bot"),
        "bank.recipients",
        json!({"recipient_id":ID}),
    )
    .await
    .unwrap();
    let d = &list["result"]["destinations"][0];
    let mut input = json!({"vendor_id":v,"connection_id":connection,"recipient_id":ID,"method":"ach","instrument_id":ID,"instrument_fingerprint":d["fingerprint"]});
    let other = payvy_server::Actor::agent(db.add_org().await, "bot");
    assert!(
        commit(&db.state, &other, "vendor.propose_payee", input.clone())
            .await
            .is_err()
    );
    input["instrument_id"] = json!(OTHER);
    assert!(
        commit(
            &db.state,
            &agent("bot"),
            "vendor.propose_payee",
            input.clone()
        )
        .await
        .is_err()
    );
    input["instrument_id"] = json!(ID);
    let p = commit(
        &db.state,
        &agent("bot"),
        "vendor.propose_payee",
        input.clone(),
    )
    .await
    .unwrap();
    assert_eq!(p["result"]["proposal"]["destination"], "…1234");
    let accept = json!({"proposal_id":p["result"]["proposal"]["id"]});
    assert!(
        commit(
            &db.state,
            &agent("bot"),
            "vendor.accept_payee",
            accept.clone()
        )
        .await
        .is_err()
    );
    fake.data.lock().unwrap().last_four = true;
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "vendor.accept_payee",
            accept.clone()
        )
        .await
        .is_err()
    );
    fake.data.lock().unwrap().last_four = false;
    fake.data.lock().unwrap().inactive = true;
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "vendor.accept_payee",
            accept.clone()
        )
        .await
        .is_err()
    );
    fake.data.lock().unwrap().inactive = false;
    commit(&db.state, &person("robert"), "vendor.accept_payee", accept)
        .await
        .unwrap();
    let p = commit(&db.state, &agent("bot"), "vendor.propose_payee", input)
        .await
        .unwrap();
    connected(&db).await;
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "vendor.accept_payee",
            json!({"proposal_id":p["result"]["proposal"]["id"]})
        )
        .await
        .is_err()
    );
    assert_eq!(fake.data.lock().unwrap().writes, 0);
    db.drop().await;
}

#[tokio::test]
async fn renewal_preserves_refresh_deadline_and_preview_never_exchanges() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    db.state = db.state.clone().with_bank(fake.bank());
    let connection = connected(&db).await;
    *db.clock.lock().unwrap() += chrono::Duration::minutes(61);
    assert!(
        commit(&db.state, &agent("bot"), "bank.recipients", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("expired")
    );
    let input = json!({"connection_id":connection});
    commands::run(
        &db.state,
        &person("robert"),
        "bank.renew",
        input.clone(),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(fake.data.lock().unwrap().refreshes, 0);
    let who = person("robert");
    let (first, second) = tokio::join!(
        commit(&db.state, &who, "bank.renew", input.clone()),
        commit(&db.state, &who, "bank.renew", input.clone()),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let renewed = first.or(second).unwrap();
    assert!(
        renewed["events"]
            .as_array()
            .unwrap()
            .contains(&json!("bank.credentials_renewed"))
    );
    assert!(!renewed.to_string().contains("secret-"));
    commit(&db.state, &agent("bot"), "bank.recipients", json!({}))
        .await
        .unwrap();
    *db.clock.lock().unwrap() += chrono::Duration::minutes(60);
    let health = commit(&db.state, &person("robert"), "bank.renew", input)
        .await
        .unwrap();
    assert_eq!(health["result"]["health"]["reason"], "grant_expired");
    assert_eq!(fake.data.lock().unwrap().refreshes, 1);
    db.drop().await;
}

#[tokio::test]
async fn activation_cleans_up_mercury_webhook_and_polling_retains_old_payments() {
    // Cleanup failure must not undo a valid activation or lose monitoring.
    for fail_cleanup in [false, true] {
        let Some(mut db) = TestDb::new().await else {
            return;
        };
        let fake = Fake::start().await;
        fake.data.lock().unwrap().fail_webhook_delete = fail_cleanup;
        db.state = db.state.clone().with_bank(fake.bank());
        let org = payvy_store::DEFAULT_ORG;
        let old = Uuid::new_v4();
        let now = (db.state.clock)();
        let bank = db.state.bank.as_ref().unwrap();
        let sealed = bank
            .sealer
            .seal(org, &format!("bank:{old}"), "legacy-mercury-token")
            .unwrap();
        let webhook_secret = bank
            .sealer
            .seal(org, &format!("webhook:{old}"), "legacy-webhook-secret")
            .unwrap();
        // Seed a payment already submitted to Mercury, before switching banks.
        let v = vendor(&db.state, &agent("bot"), "Legacy Mercury supplier").await;
        let b = Uuid::parse_str(&bill(&db.state, &agent("bot"), &v, "BEFORE-RAMP", 1000).await)
            .unwrap();
        let mut tx = db.state.store.begin(org).await.unwrap();
        payvy_store::insert_connection(
            &mut tx,
            org,
            &payvy_store::NewConnection {
                id: old,
                provider: "mercury",
                environment: "sandbox",
                mode: "direct",
                account_id: "legacy-account",
                account_name: "Legacy Mercury",
                token_sealed: &sealed,
                created_at: now,
                created_by: "robert",
            },
        )
        .await
        .unwrap();
        payvy_store::set_connection_webhook(&mut tx, old, "legacy-hook", &webhook_secret)
            .await
            .unwrap();
        let payee = payvy_store::insert_payee_proposal(
            &mut tx,
            org,
            &payvy_store::NewPayee {
                vendor_id: Uuid::parse_str(&v).unwrap(),
                connection_id: old,
                recipient_id: "legacy-recipient",
                recipient_name: "Legacy supplier",
                method: "ach",
                destination: "…6789",
                reason: None,
                proposed_by: "robert",
                created_at: now,
            },
        )
        .await
        .unwrap();
        payvy_store::decide_payee(&mut tx, payee.id, payee.vendor_id, true, "robert", now)
            .await
            .unwrap();
        payvy_store::release_bill(&mut tx, b, "robert", now)
            .await
            .unwrap();
        let payment = payvy_store::insert_payment(
            &mut tx,
            org,
            &payvy_store::NewPayment {
                id: Uuid::new_v4(),
                bill_id: b,
                connection_id: old,
                payee_id: payee.id,
                amount_cents: 1000,
                method: "ach",
                idempotency_key: "legacy-payment",
                created_at: now,
                created_by: "robert",
            },
        )
        .await
        .unwrap();
        payvy_store::update_payment(
            &mut tx,
            payment.id,
            &payvy_store::PaymentUpdate {
                status: "pending",
                transaction_id: Some("legacy-transaction"),
                request_id: None,
                failure: None,
                link: None,
            },
            now,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let stage = ready(&db).await;
        let input = activation(&stage);
        let preview = commands::run(
            &db.state,
            &person("robert"),
            "bank.activate",
            input.clone(),
            Mode::Preview,
        )
        .await
        .unwrap();
        assert_eq!(fake.data.lock().unwrap().webhook_deletes, 0);
        commands::run(
            &db.state,
            &person("robert"),
            "bank.activate",
            input,
            Mode::Commit {
                confirm_token: Some(preview["confirm_token"].as_str().unwrap().into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(fake.data.lock().unwrap().webhook_deletes, 1);
        let mut tx = db.state.store.begin(org).await.unwrap();
        let retired = payvy_store::get_connection(&mut tx, old)
            .await
            .unwrap()
            .unwrap();
        assert!(retired.retired_at.is_some());
        assert_eq!(retired.token_sealed, sealed);
        tx.rollback().await.unwrap();
        *db.clock.lock().unwrap() += chrono::Duration::minutes(3);
        assert_eq!(
            payvy_server::payment_worker::tick(&db.state).await.unwrap(),
            1
        );
        assert_eq!(fake.data.lock().unwrap().mercury_checks, 1);
        let result = commit(
            &db.state,
            &person("robert"),
            "bill.get",
            json!({"bill_id":b}),
        )
        .await
        .unwrap();
        assert_eq!(result["result"]["bill"]["state"], "paid");
        assert_eq!(
            result["result"]["bill"]["paid_reference"],
            "legacy-transaction"
        );
        assert_eq!(fake.data.lock().unwrap().writes, 0);
        db.drop().await;
    }
}

async fn health(db: &TestDb) -> Value {
    commit(&db.state, &person("robert"), "bank.get", json!({}))
        .await
        .unwrap()["result"]["connection"]["health"]
        .clone()
}

#[tokio::test]
async fn worker_renews_before_expiry_and_serializes_replicas() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    db.state = db.state.clone().with_bank(fake.bank());
    let id = connected(&db).await;
    assert_eq!(health(&db).await["status"], "connected");
    *db.clock.lock().unwrap() += chrono::Duration::minutes(16);
    assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 1);
    assert_eq!(fake.data.lock().unwrap().tokens, 1); // health probe, no early exchange
    *db.clock.lock().unwrap() += chrono::Duration::minutes(40);
    let (a, b) = tokio::join!(
        payvy_server::ramp_worker::tick(&db.state),
        payvy_server::ramp_worker::tick(&db.state)
    );
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    assert_eq!(fake.data.lock().unwrap().tokens, 2);
    assert_eq!(health(&db).await["status"], "connected");
    assert_eq!(health(&db).await["access_expired"], false);
    commit(
        &db.state,
        &agent("bot"),
        "bank.recipients",
        json!({"recipient_id":ID}),
    )
    .await
    .unwrap();
    let other = payvy_server::Actor::agent(db.add_org().await, "bank");
    assert!(
        commit(
            &db.state,
            &other,
            "bank.maintain",
            json!({"connection_id":id})
        )
        .await
        .is_err()
    );
    // Maintenance is operational only; it never gains person-only activation rights.
    assert!(
        commit(
            &db.state,
            &agent("bank"),
            "bank.disconnect",
            json!({"provider":"ramp"})
        )
        .await
        .is_err()
    );
    commit(
        &db.state,
        &person("robert"),
        "bank.disconnect",
        json!({"provider":"ramp"}),
    )
    .await
    .unwrap();
    *db.clock.lock().unwrap() += chrono::Duration::hours(1);
    assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 0);
    assert_eq!(fake.data.lock().unwrap().tokens, 2);
    assert_eq!(fake.data.lock().unwrap().writes, 0);
    db.drop().await;
}

#[tokio::test]
async fn transient_failures_back_off_then_pause_until_a_person_resumes() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    db.state = db.state.clone().with_bank(fake.bank());
    let id = connected(&db).await;
    fake.data.lock().unwrap().token_status = Some(503);
    *db.clock.lock().unwrap() += chrono::Duration::minutes(56);
    for attempt in 1..=5 {
        let before = (db.state.clock)();
        assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 1);
        let h = health(&db).await;
        assert_eq!(
            h["status"],
            if attempt == 5 { "paused" } else { "retrying" }
        );
        assert_eq!(h["reason"], "provider_unavailable");
        assert!(!h.to_string().contains("secret"));
        assert_eq!(fake.data.lock().unwrap().tokens, attempt + 1);
        assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 0);
        assert!(
            commit(
                &db.state,
                &person("robert"),
                "bank.renew",
                json!({"connection_id":id})
            )
            .await
            .is_err()
        );
        let delay = chrono::Duration::seconds(60 << (attempt - 1));
        assert_eq!(h["retry_at"], json!(before + delay));
        *db.clock.lock().unwrap() += delay;
    }
    assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 0);
    // Agents cannot override the stop or call the person's recovery action.
    assert!(
        commit(
            &db.state,
            &agent("bot"),
            "bank.renew",
            json!({"connection_id":id})
        )
        .await
        .is_err()
    );
    assert!(
        commit(
            &db.state,
            &agent("bot"),
            "bank.maintain",
            json!({"connection_id":id})
        )
        .await
        .is_err()
    );
    fake.data.lock().unwrap().token_status = None;
    let recovered = commit(
        &db.state,
        &person("robert"),
        "bank.renew",
        json!({"connection_id":id}),
    )
    .await
    .unwrap();
    assert_eq!(recovered["result"]["health"]["status"], "connected");
    assert_eq!(health(&db).await["reason"], Value::Null);
    db.drop().await;
}

#[tokio::test]
async fn rate_limits_are_persisted_and_previews_never_exchange() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    db.state = db.state.clone().with_bank(fake.bank());
    let id = connected(&db).await;
    *db.clock.lock().unwrap() += chrono::Duration::minutes(56);
    {
        let mut d = fake.data.lock().unwrap();
        d.token_status = Some(429);
        d.retry_after = Some(600);
    }
    fake.data.lock().unwrap().advance_on_token = Some(db.clock.clone());
    commands::run(
        &db.state,
        &agent("bank"),
        "bank.maintain",
        json!({"connection_id":id}),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(fake.data.lock().unwrap().tokens, 1);
    assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 1);
    let h = health(&db).await;
    assert_eq!(h["reason"], "rate_limited");
    assert_eq!(
        h["retry_at"],
        json!((db.state.clock)() + chrono::Duration::minutes(10))
    );
    *db.clock.lock().unwrap() += chrono::Duration::minutes(9);
    assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 0);
    assert!(
        commit(
            &db.state,
            &person("robert"),
            "bank.renew",
            json!({"connection_id":id})
        )
        .await
        .is_err()
    );
    assert_eq!(fake.data.lock().unwrap().tokens, 2);
    fake.data.lock().unwrap().token_status = None;
    *db.clock.lock().unwrap() += chrono::Duration::minutes(1);
    assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 1);
    assert_eq!(health(&db).await["status"], "connected");
    db.drop().await;
}

#[tokio::test]
async fn rejected_grants_stop_and_read_errors_never_rewrite_health() {
    for status in [400, 401, 403] {
        let Some(mut db) = TestDb::new().await else {
            return;
        };
        let fake = Fake::start().await;
        db.state = db.state.clone().with_bank(fake.bank());
        let id = connected(&db).await;
        fake.data.lock().unwrap().token_status = Some(status);
        *db.clock.lock().unwrap() += chrono::Duration::minutes(56);
        assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 1);
        let h = health(&db).await;
        assert_eq!(h["status"], "needs_reconnection");
        assert_eq!(h["reason"], "authorization_rejected");
        assert!(!h.to_string().contains("private-grant-error"));
        assert!(
            commit(&db.state, &agent("bot"), "bank.recipients", json!({}))
                .await
                .is_err()
        );
        *db.clock.lock().unwrap() += chrono::Duration::hours(1);
        assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 0);
        assert!(
            commit(
                &db.state,
                &person("robert"),
                "bank.renew",
                json!({"connection_id":id})
            )
            .await
            .is_err()
        );
        assert_eq!(fake.data.lock().unwrap().tokens, 2);
        assert_eq!(health(&db).await["status"], "needs_reconnection");
        db.drop().await;
    }
}

#[tokio::test]
async fn a_refresh_is_saved_even_if_the_following_health_probe_fails() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    db.state = db.state.clone().with_bank(fake.bank());
    connected(&db).await;
    *db.clock.lock().unwrap() += chrono::Duration::minutes(56);
    fake.data.lock().unwrap().business_status = Some(503);
    assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 1);
    assert_eq!(health(&db).await["status"], "retrying");
    assert_eq!(fake.data.lock().unwrap().tokens, 2);
    fake.data.lock().unwrap().business_status = None;
    *db.clock.lock().unwrap() += chrono::Duration::minutes(1);
    assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 1);
    assert_eq!(health(&db).await["status"], "connected");
    assert_eq!(fake.data.lock().unwrap().tokens, 2); // no redundant token exchange
    db.drop().await;
}

#[tokio::test]
async fn malformed_tokens_revoked_access_and_missing_refresh_grants_are_safe() {
    for scenario in ["malformed", "revoked", "no_refresh"] {
        let Some(mut db) = TestDb::new().await else {
            return;
        };
        let fake = Fake::start().await;
        db.state = db.state.clone().with_bank(fake.bank());
        if scenario == "no_refresh" {
            fake.data.lock().unwrap().missing_refresh = true;
        }
        connected(&db).await;
        *db.clock.lock().unwrap() += chrono::Duration::minutes(56);
        if scenario == "malformed" {
            fake.data.lock().unwrap().malformed_token = true;
        }
        if scenario == "revoked" {
            fake.data.lock().unwrap().business_status = Some(401);
        }
        assert_eq!(payvy_server::ramp_worker::tick(&db.state).await.unwrap(), 1);
        let h = health(&db).await;
        assert_eq!(
            h["status"],
            if scenario == "malformed" {
                "retrying"
            } else {
                "needs_reconnection"
            }
        );
        assert_eq!(
            h["reason"],
            match scenario {
                "malformed" => "invalid_response",
                "revoked" => "authorization_rejected",
                _ => "no_refresh_grant",
            }
        );
        assert!(!h.to_string().contains("secret"));
        db.drop().await;
    }
}

#[tokio::test]
async fn mapping_uses_one_vendor_and_stale_mappings_are_labeled() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let fake = Fake::start().await;
    db.state = db.state.clone().with_bank(fake.bank());
    let connection = connected(&db).await;
    let v = vendor(&db.state, &agent("bot"), "Supply Co").await;
    let r = commit(
        &db.state,
        &agent("bot"),
        "bank.recipients",
        json!({"recipient_id":ID}),
    )
    .await
    .unwrap();
    assert_eq!(fake.data.lock().unwrap().directory_reads, 0);
    assert_eq!(fake.data.lock().unwrap().vendor_reads, 1);
    let input = json!({"vendor_id":v,"connection_id":connection,"recipient_id":ID,"method":"ach","instrument_id":ID,"instrument_fingerprint":r["result"]["destinations"][0]["fingerprint"]});
    let p = commit(
        &db.state,
        &agent("bot"),
        "vendor.propose_payee",
        input.clone(),
    )
    .await
    .unwrap();
    commit(
        &db.state,
        &person("robert"),
        "vendor.accept_payee",
        json!({"proposal_id":p["result"]["proposal"]["id"]}),
    )
    .await
    .unwrap();
    commit(&db.state, &agent("bot"), "vendor.propose_payee", input)
        .await
        .unwrap();
    assert_eq!(fake.data.lock().unwrap().directory_reads, 0);
    let detail = commit(
        &db.state,
        &agent("bot"),
        "vendor.get",
        json!({"vendor_id":v}),
    )
    .await
    .unwrap();
    assert!(
        detail["result"]["payees"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["is_current_connection"] == true)
    );
    connected(&db).await;
    let detail = commit(
        &db.state,
        &agent("bot"),
        "vendor.get",
        json!({"vendor_id":v}),
    )
    .await
    .unwrap();
    assert!(
        detail["result"]["payees"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["is_current_connection"] == false)
    );
    let inbox = commit(&db.state, &person("robert"), "inbox.get", json!({}))
        .await
        .unwrap();
    assert_eq!(
        inbox["result"]["payee_changes"][0]["is_current_connection"],
        false
    );
    db.drop().await;
}

/// Stall each HTTP phase, rather than rely on wall-clock sleeps. Bank writes
/// must finish before the stalled response is released, and that response must
/// not update the retired connection's credentials, health, or audit events.
#[tokio::test]
async fn slow_maintenance_does_not_block_payees_or_connection_changes() {
    for path in ["/developer/v1/token", "/developer/v1/business"] {
        let Some(mut db) = TestDb::new().await else {
            return;
        };
        let fake = Fake::start().await;
        db.state = db.state.clone().with_bank(fake.bank());
        let id = connected(&db).await;
        let v = vendor(&db.state, &agent("bot"), "Local supplier").await;
        let list = commit(
            &db.state,
            &agent("bot"),
            "bank.recipients",
            json!({"recipient_id":ID}),
        )
        .await
        .unwrap();
        let fingerprint = list["result"]["destinations"][0]["fingerprint"].clone();
        let before_health = health(&db).await;
        let mut tx = db
            .state
            .store
            .begin(payvy_store::DEFAULT_ORG)
            .await
            .unwrap();
        let before_credentials = payvy_store::get_connection(&mut tx, id.parse().unwrap())
            .await
            .unwrap()
            .unwrap()
            .token_sealed;
        tx.rollback().await.unwrap();
        *db.clock.lock().unwrap() += chrono::Duration::minutes(56);
        let gate = Arc::new(Gate::default());
        fake.data.lock().unwrap().pause = Some((path.into(), gate.clone()));
        let state = db.state.clone();
        let original_id = id.clone();
        let check = tokio::spawn(async move {
            commit(
                &state,
                &agent("bank"),
                "bank.maintain",
                json!({"connection_id":original_id}),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
            .await
            .expect("maintenance reached stalled Ramp request");
        let replacement = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let proposal = commit(
                &db.state,
                &agent("bot"),
                "vendor.propose_payee",
                json!({
                    "vendor_id":v,"connection_id":id,"recipient_id":ID,"method":"ach",
                    "instrument_id":ID,"instrument_fingerprint":fingerprint
                }),
            )
            .await
            .unwrap();
            commit(
                &db.state,
                &person("robert"),
                "vendor.accept_payee",
                json!({"proposal_id":proposal["result"]["proposal"]["id"]}),
            )
            .await
            .unwrap();
            commit(
                &db.state,
                &person("robert"),
                "bank.disconnect",
                json!({"provider":"ramp"}),
            )
            .await
            .unwrap();
            connected(&db).await
        })
        .await
        .expect("bank operations must not wait for maintenance HTTP");
        assert_ne!(replacement, id);
        let replacement_health = health(&db).await;
        gate.resume.notify_one();
        let error = check.await.unwrap().unwrap_err();
        assert!(
            matches!(error, commands::CommandError::Conflict(_)),
            "{error}"
        );
        assert_eq!(health(&db).await, replacement_health);
        let mut tx = db
            .state
            .store
            .begin(payvy_store::DEFAULT_ORG)
            .await
            .unwrap();
        let original = payvy_store::get_connection(&mut tx, id.parse().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(original.retired_at.is_some());
        assert_eq!(original.token_sealed, before_credentials);
        let h = payvy_store::ramp::health(&mut tx, original.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(json!(h.checked_at), before_health["checked_at"]);
        assert_eq!(h.health_status, "connected");
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE entity_id=$1 AND kind IN ('bank.credentials_renewed','bank.connection_checked')")
            .bind(original.id).fetch_one(&mut *tx).await.unwrap();
        assert_eq!(events, 0);
        tx.rollback().await.unwrap();
        assert_eq!(fake.data.lock().unwrap().writes, 0);
        db.drop().await;
    }
}
