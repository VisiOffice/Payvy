//! Shared test harness: a fresh database per test, a clock the test moves,
//! and helpers that run commands through the real dispatcher.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use payvy_server::commands::{self, CommandError, Mode};
use payvy_server::{Actor, AppState};
use serde_json::{Value, json};
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

pub const ADMIN_KEY: &str = "test-admin-key-0123456789abcdef0123456789";

pub struct TestDb {
    pub state: AppState,
    pub clock: Arc<Mutex<DateTime<Utc>>>,
    admin_url: String,
    name: String,
}

impl TestDb {
    /// None (and the test returns early) when no test database is configured.
    pub async fn new() -> Option<Self> {
        let (Ok(admin_url), Ok(app_url)) = (
            std::env::var("PAYVY_TEST_ADMIN_URL"),
            std::env::var("PAYVY_TEST_APP_URL"),
        ) else {
            eprintln!("skipping: PAYVY_TEST_ADMIN_URL / PAYVY_TEST_APP_URL not set");
            return None;
        };
        let name = format!("payvy_test_{}", Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&admin_url)
            .await
            .expect("test Postgres reachable (docker compose up -d)");
        // The name is a generated UUID, so formatting it into SQL is safe.
        admin
            .execute(sqlx::AssertSqlSafe(format!(
                "CREATE DATABASE {name} OWNER payvy"
            )))
            .await
            .unwrap();
        let store = payvy_store::Store::connect(&format!("{app_url}/{name}"))
            .await
            .unwrap();
        let clock = Arc::new(Mutex::new(
            DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
        ));
        let c = clock.clone();
        let state =
            AppState::new(store, ADMIN_KEY).with_clock(Arc::new(move || *c.lock().unwrap()));
        Some(Self {
            state,
            clock,
            admin_url,
            name,
        })
    }

    /// With document storage, when PAYVY_TEST_S3_* points at a store (docker
    /// compose runs SeaweedFS). None, and the test returns early, otherwise.
    pub async fn with_storage() -> Option<Self> {
        let var = |name| std::env::var(name).ok();
        let (Some(endpoint), Some(access_key), Some(secret_key)) = (
            var("PAYVY_TEST_S3_ENDPOINT"),
            var("PAYVY_TEST_S3_ACCESS_KEY"),
            var("PAYVY_TEST_S3_SECRET_KEY"),
        ) else {
            // CI fails on this line, so storage tests can't pass by not running.
            eprintln!("skipping: PAYVY_TEST_S3_* not set");
            return None;
        };
        let config = payvy_storage::Config {
            endpoint,
            bucket: "payvy-test".into(),
            region: "us-east-1".into(),
            access_key,
            secret_key,
            path_style: true,
        };
        let mut db = Self::new().await?;
        let storage = payvy_storage::Storage::new(&config).unwrap();
        storage.ensure_bucket().await.unwrap();
        db.state = db.state.clone().with_storage(storage);
        Some(db)
    }

    pub fn advance(&self, days: i64) {
        *self.clock.lock().unwrap() += Duration::days(days);
    }

    /// A second organization, created the way an admin would.
    pub async fn add_org(&self) -> Uuid {
        let id = Uuid::new_v4();
        let url = self.admin_url.rsplit_once('/').unwrap().0.to_owned() + "/" + &self.name;
        let mut conn = PgConnection::connect(&url).await.unwrap();
        sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'Other')")
            .bind(id)
            .execute(&mut conn)
            .await
            .unwrap();
        id
    }

    pub async fn drop(self) {
        drop(self.state);
        if let Ok(mut admin) = PgConnection::connect(&self.admin_url).await {
            let sql = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name);
            let _ = admin.execute(sqlx::AssertSqlSafe(sql)).await;
        }
    }
}

/// Existing business-rule fixtures use a trusted operator. Role/security tests
/// explicitly use Actor::person and real sessions/keys to exercise authorization.
pub fn person(name: &str) -> Actor {
    Actor::administrator(payvy_store::DEFAULT_ORG, name)
}

pub fn agent(name: &str) -> Actor {
    Actor::agent(payvy_store::DEFAULT_ORG, name)
}

pub async fn commit(
    state: &AppState,
    who: &Actor,
    name: &str,
    input: Value,
) -> Result<Value, CommandError> {
    commands::run(
        state,
        who,
        name,
        input,
        Mode::Commit {
            confirm_token: None,
        },
    )
    .await
}

/// Preview, then commit with the token the preview returned.
pub async fn confirm(
    state: &AppState,
    who: &Actor,
    name: &str,
    input: Value,
) -> Result<Value, CommandError> {
    let preview = commands::run(state, who, name, input.clone(), Mode::Preview).await?;
    let token = preview["confirm_token"]
        .as_str()
        .expect("a confirmed command returns a token")
        .to_owned();
    commands::run(
        state,
        who,
        name,
        input,
        Mode::Commit {
            confirm_token: Some(token),
        },
    )
    .await
}

pub async fn vendor(state: &AppState, who: &Actor, name: &str) -> String {
    let v = commit(
        state,
        who,
        "vendor.create",
        json!({ "name": name, "terms_days": 30 }),
    )
    .await
    .unwrap();
    v["result"]["id"].as_str().unwrap().to_owned()
}

pub async fn bill(
    state: &AppState,
    who: &Actor,
    vendor_id: &str,
    number: &str,
    cents: i64,
) -> String {
    let b = commit(
        state,
        who,
        "bill.create",
        json!({ "vendor_id": vendor_id, "invoice_number": number, "invoice_date": "2027-01-15", "amount_cents": cents }),
    )
    .await
    .unwrap();
    b["result"]["bill"]["id"].as_str().unwrap().to_owned()
}

pub async fn status(state: &AppState, who: &Actor, bill_id: &str) -> String {
    let b = commit(state, who, "bill.get", json!({ "bill_id": bill_id }))
        .await
        .unwrap();
    b["result"]["progress"]["status"]
        .as_str()
        .unwrap_or("closed")
        .to_owned()
}

/// Proposed by `by`, accepted by the person `robert`.
pub async fn set_workflow(state: &AppState, by: &Actor, vendor_id: Option<&str>, steps: Value) {
    let mut input = json!({ "workflow": { "steps": steps } });
    if let Some(v) = vendor_id {
        input["vendor_id"] = json!(v);
    }
    let p = commit(state, by, "workflow.propose", input).await.unwrap();
    let id = p["result"]["proposal_id"].as_str().unwrap().to_owned();
    commit(
        state,
        &person("robert"),
        "workflow.accept",
        json!({ "proposal_id": id }),
    )
    .await
    .unwrap();
}
