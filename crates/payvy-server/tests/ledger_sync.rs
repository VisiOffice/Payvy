//! The ledger catalog: `ledger.sync` reads the books' accounts, periods,
//! vendors and segments into Payvy's cache; `ledger.vendor_matches`
//! suggests each vendor's ledger vendor; a person links it with
//! `ledger.link_vendor`; and posting checks bills against the cache.
//! The ledger is the fake; the VisiBooks reads are covered in payvy-ledger.

use chrono::NaiveDate;
use payvy_ledger::{
    Catalog, CatalogAccount, CatalogPeriod, CatalogSegment, CatalogVendor, FakeLedger,
};
use payvy_server::auth::Auth;
use payvy_server::commands::CommandError;
use payvy_server::mailer::Mailer;
use payvy_server::{Bank, secrets::Sealer};
use payvy_store::{self as store, DEFAULT_ORG, NewLedgerConnection};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

mod common;
use common::{TestDb, agent, commit, person};

fn with_fake(db: &TestDb) -> (payvy_server::AppState, FakeLedger) {
    let fake = FakeLedger::new();
    let state = db
        .state
        .clone()
        .with_bank(Bank {
            sealer: Sealer::from_hex(&"5a".repeat(32)).unwrap(),
            ramp: None,
            mercury_production: "https://mercury.invalid".into(),
            mercury_sandbox: "http://127.0.0.1:9".into(),
            rho_production: "https://rho.invalid".into(),
            rho_sandbox: "http://127.0.0.1:9".into(),
            webhook_base: None,
            outbound_ips: Vec::new(),
        })
        .with_ledger(fake.clone())
        .with_auth(Auth::new(
            "https://app.payvy.example",
            Mailer::Memory(Arc::new(Mutex::new(Vec::new()))),
        ));
    (state, fake)
}

/// A connection with an empty cache and no company default account, so
/// every account a bill posts to comes from the synced catalog.
async fn connect(db: &TestDb, state: &payvy_server::AppState) {
    let now = *db.clock.lock().unwrap();
    let sealed = Sealer::from_hex(&"ab".repeat(32))
        .unwrap()
        .seal(
            DEFAULT_ORG,
            &payvy_ledger::seal_purpose("netsuite"),
            "unused",
        )
        .unwrap();
    let mut tx = state.store.begin(DEFAULT_ORG).await.unwrap();
    store::insert_ledger_connection(
        &mut tx,
        DEFAULT_ORG,
        &NewLedgerConnection {
            id: Uuid::new_v4(),
            origin: None,
            provider: "netsuite",
            environment: "production",
            account_id: "9",
            subsidiary_id: Some("9"),
            subsidiary_name: Some("Quarry LLC"),
            currency: "USD",
            status: "active",
            auth_kind: "client_credentials",
            token_sealed: &sealed,
            post_when: "approval",
            ap_account_id: Some("25"),
            defaults: &json!({}),
            bank_account_map: &json!({}),
            created_at: now,
            created_by: "ada",
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn vendor(id: &str, name: &str) -> CatalogVendor {
    CatalogVendor {
        id: id.into(),
        name: name.into(),
        email: None,
        archived: false,
        ap_owner: Some("visibooks".into()),
        default_expense_account_id: None,
        external_source: None,
        external_id: None,
    }
}

fn account(id: &str, kind: &str, active: bool) -> CatalogAccount {
    CatalogAccount {
        id: id.into(),
        number: Some(format!("{id}00")),
        name: format!("Account {id}"),
        account_type: kind.into(),
        sub_type: None,
        active,
        required_segment_type_ids: vec![],
    }
}

/// Quarry's books in miniature. Allied carries Quarry's vendor id the way
/// VisiBooks writes it; contact 42 has the bare id with no source and must
/// never match on the id alone.
fn catalog() -> Catalog {
    let mut allied = vendor("38", "Allied Waste Services");
    allied.external_source = Some("quarry".into());
    allied.external_id = Some("quarry_vendor_9".into());
    allied.default_expense_account_id = Some("66".into());
    let mut sourceless = vendor("42", "Someone Else");
    sourceless.external_id = Some("9".into());
    let mut archived = vendor("41", "Old Hauler");
    archived.archived = true;
    Catalog {
        accounts: vec![
            account("65", "expense", true),
            account("66", "expense", true),
            account("70", "expense", false),
            account("10", "asset", true),
        ],
        periods: vec![CatalogPeriod {
            id: "1".into(),
            name: "Jan 2027".into(),
            start: d(2027, 1, 1),
            end: d(2027, 1, 31),
            status: "closed".into(),
        }],
        ledger_start_date: Some(d(2026, 1, 1)),
        vendors: vec![
            allied,
            vendor("40", "ACME Supply, Inc."),
            archived,
            sourceless,
        ],
        segments: vec![CatalogSegment {
            id: "5".into(),
            name: "Phoenix".into(),
            code: Some("PHX".into()),
            active: true,
            accepts_postings: true,
            type_id: "2".into(),
            type_name: "Location".into(),
        }],
        segmentation_enabled: false,
    }
}

async fn new_vendor(state: &payvy_server::AppState, name: &str) -> Uuid {
    let v = commit(
        state,
        &person("ada"),
        "vendor.create",
        json!({ "name": name }),
    )
    .await
    .unwrap();
    Uuid::parse_str(v["result"]["id"].as_str().unwrap()).unwrap()
}

async fn matches(state: &payvy_server::AppState) -> Value {
    commit(
        state,
        &agent("claude-ap"),
        "ledger.vendor_matches",
        json!({}),
    )
    .await
    .unwrap()["result"]
        .clone()
}

fn row(m: &Value, vendor: Uuid) -> &Value {
    m["vendors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["vendor_id"] == vendor.to_string())
        .unwrap()
}

async fn bill(state: &payvy_server::AppState, vendor: Uuid, number: &str, date: &str) -> Uuid {
    let created = commit(
        state,
        &person("ada"),
        "bill.create",
        json!({ "vendor_id": vendor, "invoice_number": number,
                "invoice_date": date, "amount_cents": 10_000 }),
    )
    .await
    .unwrap();
    Uuid::parse_str(created["result"]["bill"]["id"].as_str().unwrap()).unwrap()
}

async fn post(state: &payvy_server::AppState, bill: Uuid) -> Result<Value, CommandError> {
    commit(
        state,
        &person("ada"),
        "ledger.post_bill",
        json!({ "bill_id": bill }),
    )
    .await
}

fn ledger_code(e: &CommandError) -> &'static str {
    match e {
        CommandError::Ledger { code, .. } => code,
        other => panic!("expected a ledger refusal, got {other}"),
    }
}

#[tokio::test]
async fn sync_fills_the_cache_and_a_person_links_each_vendor() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (state, fake) = with_fake(&db);
    // Without a connection there is nothing to sync.
    let e = commit(&state, &agent("claude-ap"), "ledger.sync", json!({}))
        .await
        .unwrap_err();
    assert_eq!(ledger_code(&e), "needs_auth");
    connect(&db, &state).await;

    let allied = new_vendor(&state, "Allied Waste").await;
    commit(
        &state,
        &person("ada"),
        "vendor.link_external",
        json!({ "vendor_id": allied, "source": "quarry", "external_id": "9" }),
    )
    .await
    .unwrap();
    let acme = new_vendor(&state, "Acme Supply Inc").await;
    let nobody = new_vendor(&state, "Nobody In The Books").await;

    // An agent may refresh the catalog: it only reads the books.
    fake.set_catalog(catalog());
    let synced = commit(&state, &agent("claude-ap"), "ledger.sync", json!({}))
        .await
        .unwrap();
    let r = &synced["result"];
    assert_eq!(r["counts"]["accounts"]["total"], 4);
    assert_eq!(r["counts"]["accounts"]["added"], 4);
    // One period plus the span before the books start.
    assert_eq!(r["counts"]["periods"]["total"], 2);
    assert_eq!(r["counts"]["vendors"]["total"], 4);
    assert_eq!(r["counts"]["segments"]["total"], 1);
    assert_eq!(r["vendors"]["linked"], 0);
    assert_eq!(r["vendors"]["suggested"], 2);
    assert_eq!(r["vendors"]["unmatched"], 1);
    assert!(
        fake.calls().iter().all(|c| c == "catalog"),
        "{:?}",
        fake.calls()
    );

    // Allied matches by Quarry's id (source and id together), not by name;
    // Acme by name; the sourceless "9" never matches Allied.
    let m = matches(&state).await;
    assert_eq!(row(&m, allied)["suggestion"]["external_id"], "38");
    assert_eq!(row(&m, allied)["suggestion"]["reason"], "external_id");
    assert_eq!(row(&m, acme)["suggestion"]["external_id"], "40");
    assert_eq!(row(&m, acme)["suggestion"]["reason"], "name");
    assert!(row(&m, nobody)["suggestion"].is_null());
    // The archived vendor isn't offered.
    assert!(
        !m["ledger_vendors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["external_id"] == "41")
    );

    // A bill for an unlinked vendor doesn't post.
    let unlinked = bill(&state, allied, "A-1", "2027-02-10").await;
    let e = post(&state, unlinked).await.unwrap_err();
    assert_eq!(ledger_code(&e), "vendor_unlinked");

    // Only a person links.
    let e = commit(
        &state,
        &agent("claude-ap"),
        "ledger.link_vendor",
        json!({ "vendor_id": allied, "external_id": "38" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::PersonOnly(_)), "{e}");
    let linked = commit(
        &state,
        &person("ada"),
        "ledger.link_vendor",
        json!({ "vendor_id": allied, "external_id": "38" }),
    )
    .await
    .unwrap();
    assert_eq!(linked["result"]["ledger_vendor"], "Allied Waste Services");
    // The same link again changes nothing, so it is refused.
    let e = commit(
        &state,
        &person("ada"),
        "ledger.link_vendor",
        json!({ "vendor_id": allied, "external_id": "38" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    // One ledger vendor, one Payvy vendor.
    let e = commit(
        &state,
        &person("ada"),
        "ledger.link_vendor",
        json!({ "vendor_id": acme, "external_id": "38" }),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    // Archived and unknown ledger vendors are refused.
    for id in ["41", "999"] {
        let e = commit(
            &state,
            &person("ada"),
            "ledger.link_vendor",
            json!({ "vendor_id": acme, "external_id": id }),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, CommandError::Invalid(_)), "{id}: {e}");
    }

    // Linked, the bill posts, to Allied's default expense account from the
    // books (the company has no default of its own).
    post(&state, unlinked).await.unwrap();
    assert!(
        fake.calls()
            .iter()
            .any(|c| c == &format!("post_bill payvy:bill:{unlinked}")),
        "{:?}",
        fake.calls()
    );

    // Once a bill is posted under the link, it can't change or be removed.
    for target in [json!("40"), Value::Null] {
        let e = commit(
            &state,
            &person("ada"),
            "ledger.link_vendor",
            json!({ "vendor_id": allied, "external_id": target }),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, CommandError::Conflict(_)), "{e}");
    }
    let m = matches(&state).await;
    assert_eq!(row(&m, allied)["linked"]["external_id"], "38");
    assert_eq!(m["summary"]["linked"], 1);
}

#[tokio::test]
async fn posting_checks_bills_against_the_synced_books() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (state, fake) = with_fake(&db);
    connect(&db, &state).await;
    let acme = new_vendor(&state, "Acme Supply").await;
    fake.set_catalog(catalog());
    commit(&state, &agent("claude-ap"), "ledger.sync", json!({}))
        .await
        .unwrap();
    commit(
        &state,
        &person("ada"),
        "ledger.link_vendor",
        json!({ "vendor_id": acme, "external_id": "40" }),
    )
    .await
    .unwrap();

    // Acme has no default account in the books and the company has none:
    // the line has no account.
    let b = bill(&state, acme, "C-1", "2027-02-10").await;
    let e = post(&state, b).await.unwrap_err();
    assert_eq!(ledger_code(&e), "missing_field");

    // A closed period, and a date before the books start, are refused
    // before anything is sent.
    let mut with_default = catalog();
    with_default.vendors[1].default_expense_account_id = Some("65".into());
    fake.set_catalog(with_default.clone());
    commit(&state, &agent("claude-ap"), "ledger.sync", json!({}))
        .await
        .unwrap();
    for (n, date) in [("C-2", "2027-01-20"), ("C-3", "2025-12-31")] {
        let b = bill(&state, acme, n, date).await;
        let e = post(&state, b).await.unwrap_err();
        assert_eq!(ledger_code(&e), "period_closed", "{date}");
    }

    // An inactive account, and one that isn't an expense account.
    for (n, account) in [("C-4", "70"), ("C-5", "10")] {
        let mut books = with_default.clone();
        books.vendors[1].default_expense_account_id = Some(account.into());
        fake.set_catalog(books);
        commit(&state, &agent("claude-ap"), "ledger.sync", json!({}))
            .await
            .unwrap();
        let b = bill(&state, acme, n, "2027-02-10").await;
        let e = post(&state, b).await.unwrap_err();
        assert_eq!(ledger_code(&e), "unknown_account", "{account}");
    }

    // A vendor that leaves the books is retired from the cache, not deleted.
    let mut gone = with_default.clone();
    gone.vendors.retain(|v| v.id != "40");
    fake.set_catalog(gone);
    let r = commit(&state, &agent("claude-ap"), "ledger.sync", json!({}))
        .await
        .unwrap();
    assert_eq!(r["result"]["counts"]["vendors"]["retired"], 1);
    let m = matches(&state).await;
    assert_eq!(row(&m, acme)["linked"]["archived"], true);

    // Back in the books with a usable account, the bill posts.
    fake.set_catalog(with_default);
    commit(&state, &agent("claude-ap"), "ledger.sync", json!({}))
        .await
        .unwrap();
    let b = bill(&state, acme, "C-6", "2027-02-10").await;
    post(&state, b).await.unwrap();
}
