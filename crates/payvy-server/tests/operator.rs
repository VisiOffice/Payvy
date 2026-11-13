//! Step 2 operator API and multi-company provisioning integration tests:
//! - `org.create` transactional provisioning, defaults, audit events, and web sign-in
//! - Duplicate submissions (sequential, normalized inputs, concurrent races, conflicts, preview)
//! - Multi-company isolation (RLS, API keys, confirm tokens, and per-org vs cross-org AI usage)
//! - Bidirectional credential separation (`/operator/*` vs `/api/*` and `/mcp`)
//! - `payvy org` and `--org` CLI subcommands

use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use payvy_domain::ai_usage::{TokenUsage, compute_cost_microcents};
use payvy_domain::workflow::DEFAULT_AUTO_PAY_LIMIT_CENTS;
use payvy_extract::REQUEST_TIMEOUT_SECS;
use payvy_server::auth::{Auth, CLIENT_HEADER};
use payvy_server::mailer::{Mail, Mailer};
use payvy_store::{
    self as store, ActorKind, CompleteAiUsageAttempt, NewAiUsageAttempt, NewDocument,
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, Row};
use tower::ServiceExt;
use uuid::Uuid;

mod common;
use common::{ADMIN_KEY, TestDb, bill, commit, vendor};

const OPERATOR_KEY: &str = "test-operator-key-9876543210fedcba9876543210";
const APP_URL: &str = "https://app.payvy.example";

struct Res {
    status: StatusCode,
    cookie: Option<String>,
    body: Value,
}

async fn call(state: &payvy_server::AppState, req: Request<Body>) -> Res {
    let res = payvy_server::router(state.clone())
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

fn bearer_get(path: &str, token: &str) -> Request<Body> {
    Request::get(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn bearer_post(path: &str, token: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

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
    let at = mail.text.find("#token=").expect("sign-in link in mail") + "#token=".len();
    mail.text[at..at + 64].to_owned()
}

fn session_cookie(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_owned()
}

async fn sign_in_cookie(
    state: &payvy_server::AppState,
    sent: &Arc<Mutex<Vec<Mail>>>,
    email: &str,
) -> String {
    let before = sent.lock().unwrap().len();
    let link_res = call(
        state,
        Request::post("/auth/link")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "email": email }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(link_res.status, StatusCode::ACCEPTED);
    let mail = mails(sent, before + 1).await.pop().unwrap();
    let ses_res = call(
        state,
        Request::post("/auth/session")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "token": token_in(&mail) }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(ses_res.status, StatusCode::OK, "{}", ses_res.body);
    session_cookie(&ses_res.cookie.unwrap())
}

fn cookie_post(path: &str, cookie: &str, with_client_header: bool, body: Value) -> Request<Body> {
    let mut builder = Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie);
    if with_client_header {
        builder = builder.header(CLIENT_HEADER, "web");
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn org_create_provisions_admin_audit_events_sign_in_and_defaults() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    db.state = db
        .state
        .with_operator_key(OPERATOR_KEY)
        .with_auth(Auth::new(APP_URL, Mailer::Memory(sent.clone())));

    let listed_cmds = call(&db.state, bearer_get("/operator/commands", OPERATOR_KEY)).await;
    assert_eq!(listed_cmds.status, StatusCode::OK);
    let cmd_names: Vec<&str> = listed_cmds.body["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        cmd_names,
        vec![
            "org.create",
            "org.list",
            "usage.summary",
            "entitlement.set",
            "entitlement.get"
        ]
    );

    let res = call(
        &db.state,
        bearer_post(
            "/operator/commands/org.create",
            OPERATOR_KEY,
            json!({
                "input": {
                    "idempotency_key": "signup-acme-1",
                    "name": "  Acme Fabrication  ",
                    "admin_email": "  Alice+Founder@Acme.Example ",
                    "admin_name": "  Alice Smith  "
                }
            }),
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["command"], "org.create");
    assert_eq!(res.body["mode"], "commit");
    assert_eq!(res.body["events"], json!(["org.created", "person.added"]));

    let r = &res.body["result"];
    assert_eq!(r["created"], true);
    assert_eq!(r["organization"]["name"], "Acme Fabrication");
    let org_id: Uuid = r["organization"]["id"].as_str().unwrap().parse().unwrap();
    assert_ne!(org_id, store::DEFAULT_ORG);
    assert_eq!(r["admin"]["actor"], "alice");
    assert_eq!(r["admin"]["email"], "alice+founder@acme.example");
    assert_eq!(r["admin"]["name"], "Alice Smith");
    assert_eq!(r["admin"]["created_by"], "operator");
    assert_eq!(r["admin"]["roles"], json!(["admin"]));
    assert_eq!(
        r["defaults"],
        json!({
            "workflow": "builtin",
            "auto_pay_limit_cents": DEFAULT_AUTO_PAY_LIMIT_CENTS,
            "purchase_orders_enabled": false,
            "notifications_enabled": false,
        })
    );

    // Sign in as the newly provisioned Admin via magic link and verify live defaults.
    let cookie = sign_in_cookie(&db.state, &sent, "alice+founder@acme.example").await;

    let org_get = call(
        &db.state,
        cookie_post("/api/commands/org.get", &cookie, true, json!({})),
    )
    .await;
    assert_eq!(org_get.status, StatusCode::OK);
    assert_eq!(
        org_get.body["result"]["organization"]["name"],
        "Acme Fabrication"
    );

    let wf = call(
        &db.state,
        cookie_post("/api/commands/workflow.get", &cookie, true, json!({})),
    )
    .await;
    assert_eq!(wf.status, StatusCode::OK);
    assert_eq!(wf.body["result"]["source"], "builtin");

    let limits = call(
        &db.state,
        cookie_post("/api/commands/auto_pay.limits", &cookie, true, json!({})),
    )
    .await;
    assert_eq!(limits.status, StatusCode::OK);
    assert_eq!(
        limits.body["result"]["company"]["limit_cents"],
        DEFAULT_AUTO_PAY_LIMIT_CENTS
    );
    assert_eq!(limits.body["result"]["company"]["source"], "default");

    let po = call(
        &db.state,
        cookie_post("/api/commands/po.settings", &cookie, true, json!({})),
    )
    .await;
    assert_eq!(po.status, StatusCode::OK);
    assert_eq!(po.body["result"]["enabled"], false);

    let notif = call(
        &db.state,
        cookie_post(
            "/api/commands/notification.settings",
            &cookie,
            true,
            json!({}),
        ),
    )
    .await;
    assert_eq!(notif.status, StatusCode::OK);
    assert_eq!(notif.body["result"]["settings"]["enabled"], false);

    let events = call(
        &db.state,
        cookie_post("/api/commands/event.list", &cookie, true, json!({})),
    )
    .await;
    assert_eq!(events.status, StatusCode::OK);
    let kinds: Vec<&str> = events.body["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"org.created"), "{kinds:?}");
    assert!(kinds.contains(&"person.added"), "{kinds:?}");

    // Operator org.list sees both Default and Acme Fabrication.
    let orgs = call(
        &db.state,
        bearer_post("/operator/commands/org.list", OPERATOR_KEY, json!({})),
    )
    .await;
    assert_eq!(orgs.status, StatusCode::OK);
    let list = orgs.body["result"]["organizations"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["id"], store::DEFAULT_ORG.to_string());
    assert_eq!(list[1]["id"], org_id.to_string());
    assert_eq!(list[1]["name"], "Acme Fabrication");
    assert_eq!(list[1]["people_count"], 1);

    db.drop().await;
}

#[tokio::test]
async fn org_create_idempotency_replay_concurrency_conflict_and_preview() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    db.state = db.state.with_operator_key(OPERATOR_KEY);

    // 1. Preview rolls back completely.
    let preview = call(
        &db.state,
        bearer_post(
            "/operator/commands/org.create",
            OPERATOR_KEY,
            json!({
                "mode": "preview",
                "input": {
                    "idempotency_key": "idem-race-1",
                    "name": "Beta Logistics",
                    "admin_email": "bob@beta.example",
                    "admin_name": "Bob Jones"
                }
            }),
        ),
    )
    .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
    assert_eq!(preview.body["mode"], "preview");
    assert_eq!(preview.body["result"]["created"], true);

    let after_preview = call(
        &db.state,
        bearer_post("/operator/commands/org.list", OPERATOR_KEY, json!({})),
    )
    .await;
    assert_eq!(
        after_preview.body["result"]["organizations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // 2. Five concurrent duplicate submissions with equivalent normalized inputs.
    let s = db.state.clone();
    let req_for = |email: &str, name: &str| {
        bearer_post(
            "/operator/commands/org.create",
            OPERATOR_KEY,
            json!({
                "input": {
                    "idempotency_key": "idem-race-1",
                    "name": name,
                    "admin_email": email,
                    "admin_name": "  Bob Jones "
                }
            }),
        )
    };
    let (r1, r2, r3, r4, r5) = tokio::join!(
        call(&s, req_for("bob@beta.example", "Beta Logistics")),
        call(&s, req_for(" BOB@BETA.EXAMPLE ", "  Beta Logistics  ")),
        call(&s, req_for("Bob@Beta.Example", "Beta Logistics")),
        call(&s, req_for("bob@beta.example", "Beta Logistics")),
        call(&s, req_for("bob@beta.example", "Beta Logistics")),
    );
    let responses = [r1, r2, r3, r4, r5];
    for r in &responses {
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }
    let created_count = responses
        .iter()
        .filter(|r| r.body["result"]["created"] == true)
        .count();
    assert_eq!(created_count, 1, "exactly one concurrent call creates");

    let org_id = responses[0].body["result"]["organization"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let admin_id = responses[0].body["result"]["admin"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for r in &responses {
        assert_eq!(r.body["result"]["organization"]["id"], org_id);
        assert_eq!(r.body["result"]["admin"]["id"], admin_id);
        if r.body["result"]["created"] == false {
            assert_eq!(r.body["events"], json!([]));
        } else {
            assert_eq!(r.body["events"], json!(["org.created", "person.added"]));
        }
    }

    // 3. Sequential replay returns created: false and the same org/admin.
    let replay = call(&db.state, req_for("bob@beta.example", "Beta Logistics")).await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.body["result"]["created"], false);
    assert_eq!(replay.body["result"]["organization"]["id"], org_id);
    assert_eq!(replay.body["result"]["admin"]["roles"], json!(["admin"]));

    // Only Default + Beta Logistics exist.
    let orgs = call(
        &db.state,
        bearer_post("/operator/commands/org.list", OPERATOR_KEY, json!({})),
    )
    .await;
    assert_eq!(
        orgs.body["result"]["organizations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // 4. Reusing idempotency_key with any different parameter returns 409 Conflict.
    for bad_input in [
        json!({
            "idempotency_key": "idem-race-1",
            "name": "Different Name",
            "admin_email": "bob@beta.example",
            "admin_name": "Bob Jones"
        }),
        json!({
            "idempotency_key": "idem-race-1",
            "name": "Beta Logistics",
            "admin_email": "other@beta.example",
            "admin_name": "Bob Jones"
        }),
        json!({
            "idempotency_key": "idem-race-1",
            "name": "Beta Logistics",
            "admin_email": "bob@beta.example",
            "admin_name": "Robert Jones"
        }),
        json!({
            "idempotency_key": "idem-race-1",
            "name": "Beta Logistics",
            "admin_email": "bob@beta.example",
            "admin_name": "Bob Jones",
            "admin_actor": "bobby"
        }),
    ] {
        let conflict = call(
            &db.state,
            bearer_post(
                "/operator/commands/org.create",
                OPERATOR_KEY,
                json!({ "input": bad_input }),
            ),
        )
        .await;
        assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.body);
        assert_eq!(conflict.body["error"]["code"], "conflict");
    }

    db.drop().await;
}

#[tokio::test]
async fn provisioned_companies_are_isolated_across_data_confirm_tokens_and_ai_usage() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    db.state = db.state.with_operator_key(OPERATOR_KEY);

    let create_co = |idem: &str, name: &str, email: &str, admin_name: &str| {
        bearer_post(
            "/operator/commands/org.create",
            OPERATOR_KEY,
            json!({
                "input": {
                    "idempotency_key": idem,
                    "name": name,
                    "admin_email": email,
                    "admin_name": admin_name
                }
            }),
        )
    };
    let co_a_res = call(
        &db.state,
        create_co("co-a", "Northwind Quarry", "dana@northwind.example", "Dana"),
    )
    .await;
    let co_b_res = call(
        &db.state,
        create_co(
            "co-b",
            "Southridge Paving",
            "evan@southridge.example",
            "Evan",
        ),
    )
    .await;
    assert_eq!(co_a_res.status, StatusCode::OK);
    assert_eq!(co_b_res.status, StatusCode::OK);

    let org_a: Uuid = co_a_res.body["result"]["organization"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let org_b: Uuid = co_b_res.body["result"]["organization"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (key_a, hash_a) = payvy_server::new_api_key().unwrap();
    let (key_b, hash_b) = payvy_server::new_api_key().unwrap();
    db.state
        .store
        .insert_api_key(org_a, "dana", ActorKind::Person, &hash_a)
        .await
        .unwrap();
    db.state
        .store
        .insert_api_key(org_b, "evan", ActorKind::Person, &hash_b)
        .await
        .unwrap();

    let actor_a = payvy_server::Actor::person(org_a, "dana");
    let actor_b = payvy_server::Actor::person(org_b, "evan");

    let vendor_a = vendor(&db.state, &actor_a, "Northwind Fuel").await;
    let bill_a = bill(&db.state, &actor_a, &vendor_a, "NW-100", 12_500).await;
    let vendor_b = vendor(&db.state, &actor_b, "Southridge Sand").await;
    let bill_b = bill(&db.state, &actor_b, &vendor_b, "SR-200", 48_000).await;

    // Company A sees only its own vendor, bill, people, and events.
    let list_va = call(
        &db.state,
        bearer_post("/api/commands/vendor.list", &key_a, json!({})),
    )
    .await;
    assert_eq!(
        list_va.body["result"]["vendors"].as_array().unwrap().len(),
        1
    );
    assert_eq!(list_va.body["result"]["vendors"][0]["id"], vendor_a);

    let cross_vendor = call(
        &db.state,
        bearer_post(
            "/api/commands/vendor.get",
            &key_a,
            json!({ "input": { "vendor_id": vendor_b } }),
        ),
    )
    .await;
    assert_eq!(cross_vendor.status, StatusCode::NOT_FOUND);

    let cross_bill = call(
        &db.state,
        bearer_post(
            "/api/commands/bill.get",
            &key_b,
            json!({ "input": { "bill_id": bill_a } }),
        ),
    )
    .await;
    assert_eq!(cross_bill.status, StatusCode::NOT_FOUND);
    let own_bill = call(
        &db.state,
        bearer_post(
            "/api/commands/bill.get",
            &key_b,
            json!({ "input": { "bill_id": bill_b } }),
        ),
    )
    .await;
    assert_eq!(own_bill.status, StatusCode::OK);

    let people_a = call(
        &db.state,
        bearer_post("/api/commands/person.list", &key_a, json!({})),
    )
    .await;
    assert_eq!(
        people_a.body["result"]["people"].as_array().unwrap().len(),
        1
    );
    assert_eq!(people_a.body["result"]["people"][0]["actor"], "dana");

    // Cross-company confirm_token replay: previewing auto_pay.raise_limit in Company A
    // must NOT allow committing the identical payload in Company B.
    let raise_input = json!({ "limit_cents": 1_000_000 });
    let preview_a = call(
        &db.state,
        bearer_post(
            "/api/commands/auto_pay.raise_limit",
            &key_a,
            json!({ "mode": "preview", "input": raise_input }),
        ),
    )
    .await;
    assert_eq!(preview_a.status, StatusCode::OK);
    let token_from_a = preview_a.body["confirm_token"].as_str().unwrap();

    let cross_commit_b = call(
        &db.state,
        bearer_post(
            "/api/commands/auto_pay.raise_limit",
            &key_b,
            json!({
                "mode": "commit",
                "confirm_token": token_from_a,
                "input": raise_input
            }),
        ),
    )
    .await;
    assert_eq!(cross_commit_b.status, StatusCode::CONFLICT);
    assert_eq!(cross_commit_b.body["error"]["code"], "bad_confirm_token");

    // Seed documents + AI usage attempts in Company A and Company B (Default has 0).
    // Identical sha256 across two companies must ALSO succeed because document uniqueness is per-org.
    let now = *db.clock.lock().unwrap();
    let doc_a = {
        let mut tx = db.state.store.begin(org_a).await.unwrap();
        let doc = store::insert_document(
            &mut tx,
            org_a,
            &NewDocument {
                sha256: "deadbeef",
                content_type: "application/pdf",
                size_bytes: 64,
                filename: "a.pdf",
                storage_key: &format!("{org_a}/a.pdf"),
                source: "upload",
                email_id: None,
                received_at: now,
                created_by: "dana",
                page_count: Some(1),
            },
        )
        .await
        .unwrap()
        .expect("document inserted in org A");
        tx.commit().await.unwrap();
        doc.id
    };
    let doc_b = {
        let mut tx = db.state.store.begin(org_b).await.unwrap();
        let doc = store::insert_document(
            &mut tx,
            org_b,
            &NewDocument {
                sha256: "deadbeef",
                content_type: "application/pdf",
                size_bytes: 64,
                filename: "b.pdf",
                storage_key: &format!("{org_b}/b.pdf"),
                source: "upload",
                email_id: None,
                received_at: now,
                created_by: "evan",
                page_count: Some(1),
            },
        )
        .await
        .unwrap()
        .expect("identical sha256 allowed in org B");
        tx.commit().await.unwrap();
        doc.id
    };

    let cross_doc = call(
        &db.state,
        bearer_post(
            "/api/commands/document.get",
            &key_b,
            json!({ "input": { "document_id": doc_a } }),
        ),
    )
    .await;
    assert_eq!(cross_doc.status, StatusCode::NOT_FOUND);

    let usage_a = TokenUsage {
        input_tokens: Some(1_000),
        output_tokens: Some(250),
        cache_read_tokens: Some(400),
        cache_write_tokens: None,
    };
    let (ver_a, cost_a) = compute_cost_microcents("claude-sonnet-5-5", &usage_a).unwrap();
    {
        let mut tx = db.state.store.begin(org_a).await.unwrap();
        let attempt_a = store::start_ai_usage_attempt(
            &mut tx,
            org_a,
            &NewAiUsageAttempt {
                document_id: doc_a,
                actor_kind: ActorKind::Agent,
                actor_name: "extractor",
                provider: "anthropic",
                requested_model: "claude-sonnet-5-5",
                started_at: now,
            },
        )
        .await
        .unwrap();
        store::complete_ai_usage_attempt(
            &mut tx,
            attempt_a,
            &CompleteAiUsageAttempt {
                outcome: "succeeded",
                actual_model: Some("claude-sonnet-5-5"),
                provider_request_id: Some("req_a"),
                error: None,
                usage: usage_a,
                pricing_version: Some(ver_a),
                cost_microcents: Some(cost_a),
                completed_at: now,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    let usage_b = TokenUsage {
        input_tokens: Some(2_000),
        output_tokens: Some(500),
        cache_read_tokens: None,
        cache_write_tokens: Some(100),
    };
    let (ver_b, cost_b) = compute_cost_microcents("claude-sonnet-5-5", &usage_b).unwrap();
    {
        let mut tx = db.state.store.begin(org_b).await.unwrap();
        let attempt_b = store::start_ai_usage_attempt(
            &mut tx,
            org_b,
            &NewAiUsageAttempt {
                document_id: doc_b,
                actor_kind: ActorKind::Agent,
                actor_name: "extractor",
                provider: "anthropic",
                requested_model: "claude-sonnet-5-5",
                started_at: now,
            },
        )
        .await
        .unwrap();
        store::complete_ai_usage_attempt(
            &mut tx,
            attempt_b,
            &CompleteAiUsageAttempt {
                outcome: "refused",
                actual_model: Some("claude-sonnet-5-5"),
                provider_request_id: Some("req_b"),
                error: Some("policy"),
                usage: usage_b,
                pricing_version: Some(ver_b),
                cost_microcents: Some(cost_b),
                completed_at: now,
            },
        )
        .await
        .unwrap();
        // Also add one stale pending attempt in Company B.
        store::start_ai_usage_attempt(
            &mut tx,
            org_b,
            &NewAiUsageAttempt {
                document_id: doc_b,
                actor_kind: ActorKind::Agent,
                actor_name: "extractor",
                provider: "anthropic",
                requested_model: "claude-sonnet-5-5",
                started_at: now - chrono::Duration::seconds(REQUEST_TIMEOUT_SECS + 30),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    // Company A Admin sees ONLY Company A's usage, and cannot pass org_id on /api/commands/usage.summary.
    let sum_a = call(
        &db.state,
        bearer_post("/api/commands/usage.summary", &key_a, json!({})),
    )
    .await;
    assert_eq!(sum_a.status, StatusCode::OK);
    assert_eq!(sum_a.body["result"]["attempts"], 1);
    assert_eq!(sum_a.body["result"]["successful_reads"], 1);
    assert_eq!(sum_a.body["result"]["estimated_cost_microcents"], cost_a);
    assert!(sum_a.body["result"].get("organizations").is_none());

    let sum_a_filter = call(
        &db.state,
        bearer_post(
            "/api/commands/usage.summary",
            &key_a,
            json!({ "input": { "org_id": org_b } }),
        ),
    )
    .await;
    assert_eq!(sum_a_filter.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Operator usage.summary sees cross-company totals AND per-company breakdown (including 0-attempt Default).
    let op_sum = call(
        &db.state,
        bearer_post("/operator/commands/usage.summary", OPERATOR_KEY, json!({})),
    )
    .await;
    assert_eq!(op_sum.status, StatusCode::OK, "{}", op_sum.body);
    let res = &op_sum.body["result"];
    assert_eq!(res["attempts"], 3);
    assert_eq!(res["successful_reads"], 1);
    assert_eq!(res["failed_attempts"], 1);
    assert_eq!(res["pending_attempts"], 0);
    assert_eq!(res["unknown_usage_attempts"], 1);
    assert_eq!(res["input_tokens"], 3_000);
    assert_eq!(res["output_tokens"], 750);
    assert_eq!(res["cache_read_tokens"], 400);
    assert_eq!(res["cache_write_tokens"], 100);
    assert_eq!(res["estimated_cost_microcents"], cost_a + cost_b);

    let per_org = res["organizations"].as_array().unwrap();
    assert_eq!(per_org.len(), 3);
    let find_org = |id: Uuid| {
        per_org
            .iter()
            .find(|o| o["org_id"] == id.to_string())
            .expect("organization in breakdown")
    };
    assert_eq!(find_org(store::DEFAULT_ORG)["attempts"], 0);
    assert_eq!(find_org(org_a)["attempts"], 1);
    assert_eq!(find_org(org_a)["estimated_cost_microcents"], cost_a);
    assert_eq!(find_org(org_b)["attempts"], 2);
    assert_eq!(find_org(org_b)["unknown_usage_attempts"], 1);
    assert_eq!(find_org(org_b)["estimated_cost_microcents"], cost_b);

    // Single-org filter on operator usage.summary.
    let op_single = call(
        &db.state,
        bearer_post(
            "/operator/commands/usage.summary",
            OPERATOR_KEY,
            json!({ "input": { "org_id": org_a } }),
        ),
    )
    .await;
    assert_eq!(op_single.status, StatusCode::OK);
    assert_eq!(op_single.body["result"]["attempts"], 1);
    assert_eq!(
        op_single.body["result"]["organizations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let op_missing = call(
        &db.state,
        bearer_post(
            "/operator/commands/usage.summary",
            OPERATOR_KEY,
            json!({ "input": { "org_id": Uuid::new_v4() } }),
        ),
    )
    .await;
    assert_eq!(op_missing.status, StatusCode::NOT_FOUND);

    // Bare connection as the non-superuser `payvy` role fails closed on `ai_usage_attempts`
    // (0 rows both before and after `payvy.org_id` is reset to ''), and `payvy.operator`
    // is SELECT-only (cannot delete or insert rows).
    let mut bare = PgConnection::connect(&db.app_url).await.unwrap();
    let initial_count: i64 = sqlx::query("SELECT COUNT(*)::bigint AS n FROM ai_usage_attempts")
        .fetch_one(&mut bare)
        .await
        .unwrap()
        .get("n");
    assert_eq!(initial_count, 0);

    sqlx::query("BEGIN").execute(&mut bare).await.unwrap();
    sqlx::query("SELECT set_config('payvy.org_id', $1, true)")
        .bind(org_a.to_string())
        .execute(&mut bare)
        .await
        .unwrap();
    sqlx::query("COMMIT").execute(&mut bare).await.unwrap();

    let after_reset_count: i64 = sqlx::query("SELECT COUNT(*)::bigint AS n FROM ai_usage_attempts")
        .fetch_one(&mut bare)
        .await
        .unwrap()
        .get("n");
    assert_eq!(after_reset_count, 0);

    sqlx::query("BEGIN").execute(&mut bare).await.unwrap();
    sqlx::query("SELECT set_config('payvy.operator', 'true', true)")
        .execute(&mut bare)
        .await
        .unwrap();
    let deleted = sqlx::query("DELETE FROM ai_usage_attempts")
        .execute(&mut bare)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(
        deleted, 0,
        "operator_ai_usage_read is SELECT-only; DELETE must see 0 rows"
    );
    sqlx::query("ROLLBACK").execute(&mut bare).await.unwrap();
    drop(bare);

    db.drop().await;
}

#[tokio::test]
async fn operator_and_company_credentials_reject_cross_surface_use() {
    let Some(mut db) = TestDb::new().await else {
        return;
    };
    let sent = Arc::new(Mutex::new(Vec::new()));
    let web_dir = std::env::temp_dir().join(format!("payvy-op-web-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&web_dir).unwrap();
    std::fs::write(
        web_dir.join("index.html"),
        "<!doctype html><title>Payvy SPA</title>",
    )
    .unwrap();
    db.state = db
        .state
        .with_web(&web_dir)
        .with_auth(Auth::new(APP_URL, Mailer::Memory(sent.clone())));

    // Normal SPA routes serve index.html when web_dir is set...
    let spa_res = payvy_server::router(db.state.clone())
        .oneshot(Request::get("/bills").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(spa_res.status(), StatusCode::OK);
    let spa_bytes = spa_res.into_body().collect().await.unwrap().to_bytes();
    assert!(
        String::from_utf8_lossy(&spa_bytes).contains("Payvy SPA"),
        "expected SPA index.html"
    );

    // 1. Without PAYVY_OPERATOR_KEY configured, /operator and /operator/* answer 404 not_configured JSON (never index.html).
    for path in [
        "/operator",
        "/operator/",
        "/operator/commands",
        "/operator/unknown",
    ] {
        let unconf = call(&db.state, bearer_get(path, OPERATOR_KEY)).await;
        assert_eq!(unconf.status, StatusCode::NOT_FOUND, "path={path}");
        assert_eq!(
            unconf.body["error"]["code"], "not_configured",
            "path={path}"
        );
    }

    // Enable operator key and set up a company person, session cookie, person key, and agent key.
    db.state = db.state.with_operator_key(OPERATOR_KEY);
    commit(
        &db.state,
        &payvy_server::Actor::administrator(store::DEFAULT_ORG, "admin"),
        "person.add",
        json!({
            "email": "robert@default.example",
            "name": "Robert",
            "roles": ["admin"]
        }),
    )
    .await
    .unwrap();

    let (person_key, person_hash) = payvy_server::new_api_key().unwrap();
    let (agent_key, agent_hash) = payvy_server::new_api_key().unwrap();
    db.state
        .store
        .insert_api_key(
            store::DEFAULT_ORG,
            "robert",
            ActorKind::Person,
            &person_hash,
        )
        .await
        .unwrap();
    db.state
        .store
        .insert_api_key(store::DEFAULT_ORG, "ap-bot", ActorKind::Agent, &agent_hash)
        .await
        .unwrap();
    let cookie = sign_in_cookie(&db.state, &sent, "robert@default.example").await;

    // Operator key is never stored in api_keys.
    assert!(
        db.state
            .store
            .actor_for_key_hash(&payvy_server::hash_key(OPERATOR_KEY))
            .await
            .unwrap()
            .is_none()
    );

    // 2. Operator credential against every company endpoint -> 403 Forbidden.
    let doc_id = Uuid::new_v4();
    for req in [
        bearer_get("/api/commands", OPERATOR_KEY),
        bearer_post("/api/commands/org.get", OPERATOR_KEY, json!({})),
        bearer_post("/api/commands/usage.summary", OPERATOR_KEY, json!({})),
        bearer_get(&format!("/api/documents/{doc_id}/file"), OPERATOR_KEY),
        bearer_get(&format!("/api/documents/{doc_id}/cover"), OPERATOR_KEY),
        bearer_post(
            "/mcp",
            OPERATOR_KEY,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        ),
    ] {
        let res = call(&db.state, req).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
        assert_eq!(res.body["error"]["code"], "forbidden");
    }

    // Operator credential calling a company command on /operator/commands/* -> 403 Forbidden.
    for company_cmd in ["vendor.list", "org.get", "person.add"] {
        let res = call(
            &db.state,
            bearer_post(
                &format!("/operator/commands/{company_cmd}"),
                OPERATOR_KEY,
                json!({}),
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
        assert_eq!(res.body["error"]["code"], "forbidden");
    }

    // Unknown command or unknown subpaths under /operator never fall back to index.html.
    let unknown_op = call(
        &db.state,
        bearer_post("/operator/commands/not.a.command", OPERATOR_KEY, json!({})),
    )
    .await;
    assert_eq!(unknown_op.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown_op.body["error"]["code"], "unknown_command");

    for path in ["/operator", "/operator/", "/operator/unknown"] {
        let missing_path = call(&db.state, bearer_get(path, OPERATOR_KEY)).await;
        assert_eq!(missing_path.status, StatusCode::NOT_FOUND, "path={path}");
        assert_eq!(
            missing_path.body["error"]["code"], "not_found",
            "path={path}"
        );
    }

    // 3. Every company credential against /operator/* -> 403 Forbidden.
    for token in [ADMIN_KEY, person_key.as_str(), agent_key.as_str()] {
        for req in [
            bearer_get("/operator/commands", token),
            bearer_post("/operator/commands/org.list", token, json!({})),
            bearer_post(
                "/operator/commands/org.create",
                token,
                json!({
                    "input": {
                        "idempotency_key": "nope",
                        "name": "Nope",
                        "admin_email": "a@b.co",
                        "admin_name": "A"
                    }
                }),
            ),
            bearer_post("/operator/commands/usage.summary", token, json!({})),
        ] {
            let res = call(&db.state, req).await;
            assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
            assert_eq!(res.body["error"]["code"], "forbidden");
        }
    }

    // Browser session cookie (with or without X-Payvy-Client) against /operator/* -> 403 Forbidden.
    for with_client in [true, false] {
        let res = call(
            &db.state,
            cookie_post(
                "/operator/commands/org.list",
                &cookie,
                with_client,
                json!({}),
            ),
        )
        .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
        assert_eq!(res.body["error"]["code"], "forbidden");
    }

    // Company credentials calling operator-only commands on /api/commands/* -> 403 Forbidden.
    for op_only in ["org.create", "org.list"] {
        let res = call(
            &db.state,
            bearer_post(&format!("/api/commands/{op_only}"), ADMIN_KEY, json!({})),
        )
        .await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
        assert_eq!(res.body["error"]["code"], "forbidden");
    }

    // 4. Missing or wrong token on /operator/* -> 401 Unauthorized.
    let no_auth = call(
        &db.state,
        Request::get("/operator/commands")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(no_auth.status, StatusCode::UNAUTHORIZED);

    let wrong_key = call(&db.state, bearer_get("/operator/commands", "pv_wrong_key")).await;
    assert_eq!(wrong_key.status, StatusCode::UNAUTHORIZED);

    let _ = std::fs::remove_dir_all(&web_dir);
    db.drop().await;
}

#[tokio::test]
async fn cli_org_create_list_and_org_scoped_person_and_key_commands() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let bin = env!("CARGO_BIN_EXE_payvy");

    // 1. `payvy org create` and idempotent replay.
    let out1 = Command::new(bin)
        .env("DATABASE_URL", &db.app_url)
        .args([
            "org",
            "create",
            "--name",
            "CLI Provisioned Co",
            "--admin-email",
            "claire@clico.example",
            "--admin-name",
            "Claire",
            "--idempotency-key",
            "cli-idem-1",
        ])
        .output()
        .unwrap();
    assert!(
        out1.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let created: Value = serde_json::from_slice(&out1.stdout).unwrap();
    assert_eq!(created["created"], true);
    let cli_org_id = created["organization"]["id"].as_str().unwrap().to_owned();

    let out2 = Command::new(bin)
        .env("DATABASE_URL", &db.app_url)
        .args([
            "org",
            "create",
            "--name",
            "CLI Provisioned Co",
            "--admin-email",
            "claire@clico.example",
            "--admin-name",
            "Claire",
            "--idempotency-key",
            "cli-idem-1",
        ])
        .output()
        .unwrap();
    assert!(out2.status.success());
    let replayed: Value = serde_json::from_slice(&out2.stdout).unwrap();
    assert_eq!(replayed["created"], false);
    assert_eq!(replayed["organization"]["id"], cli_org_id);

    // 2. `payvy org list` includes the new company.
    let list_out = Command::new(bin)
        .env("DATABASE_URL", &db.app_url)
        .args(["org", "list"])
        .output()
        .unwrap();
    assert!(list_out.status.success());
    let listed: Value = serde_json::from_slice(&list_out.stdout).unwrap();
    assert!(
        listed["organizations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == cli_org_id && o["name"] == "CLI Provisioned Co")
    );

    // 3. `payvy person add --org <id>` and `payvy key create --org <id>` scope to that org.
    let person_out = Command::new(bin)
        .env("DATABASE_URL", &db.app_url)
        .args([
            "person",
            "add",
            "--org",
            &cli_org_id,
            "--email",
            "clerk@clico.example",
            "--name",
            "Clerk",
            "--roles",
            "ap_clerk",
        ])
        .output()
        .unwrap();
    assert!(
        person_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&person_out.stderr)
    );

    let person_key_out = Command::new(bin)
        .env("DATABASE_URL", &db.app_url)
        .args([
            "key",
            "create",
            "--org",
            &cli_org_id,
            "--actor",
            "claire",
            "--kind",
            "person",
        ])
        .output()
        .unwrap();
    assert!(
        person_key_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&person_key_out.stderr)
    );
    let claire_key = String::from_utf8(person_key_out.stdout)
        .unwrap()
        .trim()
        .to_owned();

    let agent_key_out = Command::new(bin)
        .env("DATABASE_URL", &db.app_url)
        .args([
            "key",
            "--org",
            &cli_org_id,
            "create",
            "--actor",
            "cli-agent",
            "--kind",
            "agent",
        ])
        .output()
        .unwrap();
    assert!(
        agent_key_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&agent_key_out.stderr)
    );
    let agent_key = String::from_utf8(agent_key_out.stdout)
        .unwrap()
        .trim()
        .to_owned();

    // Both keys authenticate to CLI Provisioned Co, not Default.
    for k in [&claire_key, &agent_key] {
        let org_res = call(
            &db.state,
            bearer_post("/api/commands/org.get", k, json!({})),
        )
        .await;
        assert_eq!(org_res.status, StatusCode::OK, "{}", org_res.body);
        assert_eq!(org_res.body["result"]["organization"]["id"], cli_org_id);
        assert_eq!(
            org_res.body["result"]["organization"]["name"],
            "CLI Provisioned Co"
        );
    }

    let default_people = call(
        &db.state,
        bearer_post("/api/commands/person.list", ADMIN_KEY, json!({})),
    )
    .await;
    assert_eq!(
        default_people.body["result"]["people"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // Unknown --org fails cleanly.
    let unknown = Uuid::new_v4().to_string();
    let bad_org = Command::new(bin)
        .env("DATABASE_URL", &db.app_url)
        .args([
            "key", "create", "--org", &unknown, "--actor", "bot", "--kind", "agent",
        ])
        .output()
        .unwrap();
    assert!(!bad_org.status.success());
    assert!(
        String::from_utf8_lossy(&bad_org.stderr).contains("not found"),
        "stderr: {}",
        String::from_utf8_lossy(&bad_org.stderr)
    );

    // `payvy serve` rejects short or identical PAYVY_OPERATOR_KEY before connecting.
    let short_op = Command::new(bin)
        .env("DATABASE_URL", &db.app_url)
        .args([
            "serve",
            "--api-key",
            ADMIN_KEY,
            "--operator-key",
            "too-short",
        ])
        .output()
        .unwrap();
    assert!(!short_op.status.success());
    assert!(String::from_utf8_lossy(&short_op.stderr).contains("PAYVY_OPERATOR_KEY"));

    let dup_op = Command::new(bin)
        .env("DATABASE_URL", &db.app_url)
        .args(["serve", "--api-key", ADMIN_KEY, "--operator-key", ADMIN_KEY])
        .output()
        .unwrap();
    assert!(!dup_op.status.success());
    assert!(String::from_utf8_lossy(&dup_op.stderr).contains("differ from PAYVY_API_KEY"));

    db.drop().await;
}
