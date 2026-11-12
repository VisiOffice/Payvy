//! The ledger trait, through the real dispatcher and a fresh database.
//! The fake stands in for NetSuite. Nothing here opens a network connection.
mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::*;
use payvy_domain::bills::State;
use payvy_ledger::{Code, FakeLedger, LedgerError};
use payvy_server::commands::{self, CommandError, Mode};
use payvy_server::secrets::Sealer;
use payvy_store::{
    self as store, DEFAULT_ORG, NewConnection, NewLedgerConnection, NewLedgerRecord, NewPayee,
    NewPayment, PaymentUpdate,
};
use serde_json::{Value, json};
use uuid::Uuid;

const SECRET: &str = "payvy-refresh-token-do-not-log-7f3a9c";

struct Fx {
    db: TestDb,
    fake: FakeLedger,
    connection: Uuid,
    vendor: Uuid,
}

impl Fx {
    async fn open(currency: &str, defaults: Value, account: Option<Value>) -> Option<Self> {
        let mut db = TestDb::new().await?;
        let fake = FakeLedger::new();
        db.state = db.state.clone().with_ledger(fake.clone());
        let vendor = Uuid::parse_str(&vendor(&db.state, &person("ada"), "Acme").await).unwrap();
        let connection = Uuid::new_v4();
        let now = *db.clock.lock().unwrap();
        let sealed = Sealer::from_hex(&"ab".repeat(32))
            .unwrap()
            .seal(DEFAULT_ORG, &payvy_ledger::seal_purpose("netsuite"), SECRET)
            .unwrap();
        let mut tx = db.state.store.begin(DEFAULT_ORG).await.unwrap();
        store::insert_ledger_connection(
            &mut tx,
            DEFAULT_ORG,
            &NewLedgerConnection {
                id: connection,
                provider: "netsuite",
                environment: "production",
                account_id: "1234567",
                subsidiary_id: Some("1"),
                subsidiary_name: Some("Main"),
                currency,
                status: "active",
                auth_kind: "authorization_code",
                token_sealed: &sealed,
                post_when: "approval",
                ap_account_id: Some("25"),
                defaults: &defaults,
                bank_account_map: &json!({}),
                created_at: now,
                created_by: "ada",
            },
        )
        .await
        .unwrap();
        store::put_ledger_link(
            &mut tx,
            DEFAULT_ORG,
            &store::PutLedgerLink {
                connection_id: connection,
                entity: "vendor",
                payvy_id: vendor,
                external_id: Some("ns-1"),
                external_key: &payvy_ledger::vendor_key(vendor),
                status: "linked",
                error_code: None,
                error_detail: None,
                request_hash: None,
                posted_at: None,
            },
        )
        .await
        .unwrap();
        if let Some(detail) = &account {
            store::insert_ledger_record(
                &mut tx,
                DEFAULT_ORG,
                &NewLedgerRecord {
                    connection_id: connection,
                    kind: "account",
                    external_id: "65",
                    name: "Repairs",
                    parent_external_id: None,
                    inactive: false,
                    detail,
                },
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        Some(Self {
            db,
            fake,
            connection,
            vendor,
        })
    }

    async fn usd() -> Option<Self> {
        Self::open(
            "USD",
            json!({ "expense_account_id": "65" }),
            Some(json!({ "type": "expense" })),
        )
        .await
    }

    async fn invoice(&self, number: &str) -> Uuid {
        self.invoice_lines(number, 10_000, json!([])).await
    }

    async fn invoice_lines(&self, number: &str, cents: i64, lines: Value) -> Uuid {
        let mut input = json!({
            "vendor_id": self.vendor,
            "invoice_number": number,
            "invoice_date": "2027-01-15",
            "amount_cents": cents,
        });
        if lines.as_array().is_some_and(|rows| !rows.is_empty()) {
            input["lines"] = lines;
        }
        let created = commit(&self.db.state, &person("ada"), "bill.create", input)
            .await
            .unwrap();
        Uuid::parse_str(created["result"]["bill"]["id"].as_str().unwrap()).unwrap()
    }

    async fn post(&self, bill: Uuid) -> Result<Value, CommandError> {
        commit(
            &self.db.state,
            &person("ada"),
            "ledger.post_bill",
            json!({ "bill_id": bill }),
        )
        .await
    }

    async fn post_payment(&self, payment: Uuid) -> Result<Value, CommandError> {
        commit(
            &self.db.state,
            &person("ada"),
            "ledger.post_payment",
            json!({ "payment_id": payment }),
        )
        .await
    }

    async fn void(&self, bill: Uuid) -> Result<Value, CommandError> {
        confirm(
            &self.db.state,
            &person("ada"),
            "ledger.void",
            json!({ "bill_id": bill }),
        )
        .await
    }

    /// A sent Payvy payment. Returns the payment id and the bank connection id.
    async fn sent_payment(&self, bill: Uuid, cents: i64) -> (Uuid, Uuid) {
        let now = *self.db.clock.lock().unwrap();
        let bank = Uuid::new_v4();
        let payment = Uuid::new_v4();
        let mut tx = self.db.state.store.begin(DEFAULT_ORG).await.unwrap();
        let bank_row = store::insert_connection(
            &mut tx,
            DEFAULT_ORG,
            &NewConnection {
                id: bank,
                provider: "mercury",
                environment: "production",
                mode: "request",
                account_id: "acct",
                account_name: "Operating",
                token_sealed: b"bank-token-sealed",
                created_at: now,
                created_by: "ada",
            },
        )
        .await
        .unwrap();
        let payee = store::insert_payee_proposal(
            &mut tx,
            DEFAULT_ORG,
            &NewPayee {
                vendor_id: self.vendor,
                connection_id: bank_row.id,
                recipient_id: "recip",
                recipient_name: "Acme",
                method: "ach",
                destination: "1234",
                reason: None,
                proposed_by: "ada",
                created_at: now,
            },
        )
        .await
        .unwrap();
        store::insert_payment(
            &mut tx,
            DEFAULT_ORG,
            &NewPayment {
                id: payment,
                bill_id: bill,
                connection_id: bank_row.id,
                payee_id: payee.id,
                amount_cents: cents,
                method: "ach",
                idempotency_key: &payment.to_string(),
                created_at: now,
                created_by: "ada",
            },
        )
        .await
        .unwrap();
        store::update_payment(
            &mut tx,
            payment,
            &PaymentUpdate {
                status: "sent",
                request_id: None,
                transaction_id: Some("txn-1"),
                failure: None,
                link: None,
            },
            now,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (payment, bank)
    }

    /// A second sent payment on the same bank. The first must no longer be live.
    async fn sent_again(&self, earlier: Uuid, cents: i64) -> Uuid {
        let now = *self.db.clock.lock().unwrap();
        let payment = Uuid::new_v4();
        let mut tx = self.db.state.store.begin(DEFAULT_ORG).await.unwrap();
        let earlier = store::get_payment(&mut tx, earlier).await.unwrap().unwrap();
        store::insert_payment(
            &mut tx,
            DEFAULT_ORG,
            &NewPayment {
                id: payment,
                bill_id: earlier.bill_id,
                connection_id: earlier.connection_id,
                payee_id: earlier.payee_id,
                amount_cents: cents,
                method: "ach",
                idempotency_key: &payment.to_string(),
                created_at: now,
                created_by: "ada",
            },
        )
        .await
        .unwrap();
        store::update_payment(
            &mut tx,
            payment,
            &PaymentUpdate {
                status: "sent",
                request_id: None,
                transaction_id: Some("txn-2"),
                failure: None,
                link: None,
            },
            now,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        payment
    }

    async fn map_bank(&self, bank: Uuid, account: &str) {
        let mut tx = self.db.state.store.begin(DEFAULT_ORG).await.unwrap();
        store::set_ledger_bank_map(
            &mut tx,
            self.connection,
            &json!({ bank.to_string(): account }),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    async fn link(&self, entity: &str, id: Uuid) -> store::LedgerLink {
        let mut tx = self.db.state.store.begin(DEFAULT_ORG).await.unwrap();
        let link = store::ledger_link(&mut tx, self.connection, entity, id)
            .await
            .unwrap()
            .unwrap();
        tx.rollback().await.unwrap();
        link
    }
}

fn code(err: CommandError) -> &'static str {
    match err {
        CommandError::Ledger { code, .. } => code,
        other => panic!("expected a ledger error, got {other}"),
    }
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);
impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn replay_returns_the_first_id_and_a_change_conflicts() {
    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1001").await;
    let preview = commands::run(
        &fx.db.state,
        &person("ada"),
        "ledger.post_bill",
        json!({ "bill_id": bill }),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(preview["result"]["status"], "ready");
    assert!(fx.fake.calls().is_empty());

    let first = fx.post(bill).await.unwrap();
    assert_eq!(first["result"]["replay"], json!(false));
    assert_eq!(first["result"]["external_id"], "fake-bill-1");
    let again = fx.post(bill).await.unwrap();
    assert_eq!(again["result"]["replay"], json!(true));
    assert_eq!(again["result"]["external_id"], "fake-bill-1");
    assert_eq!(fx.fake.calls().len(), 1);

    let mut tx = fx.db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::set_bill_memo(&mut tx, bill, Some("changed"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    match fx.post(bill).await.unwrap_err() {
        CommandError::Ledger { code, detail } => {
            assert_eq!(code, "conflict");
            assert_eq!(detail, "This changed after it was posted");
        }
        other => panic!("expected a conflict, got {other}"),
    }
    assert_eq!(fx.fake.calls().len(), 1);
    let link = fx.link("bill", bill).await;
    assert_eq!(link.status, "posted");
    assert_eq!(link.external_id.as_deref(), Some("fake-bill-1"));
    fx.db.drop().await;
}

#[tokio::test]
async fn named_refusals_do_not_call_the_ledger() {
    let Some(fx) = Fx::usd().await else { return };
    let mut tx = fx.db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::insert_ledger_record(
        &mut tx,
        DEFAULT_ORG,
        &NewLedgerRecord {
            connection_id: fx.connection,
            kind: "period",
            external_id: "2027-01",
            name: "Jan 2027",
            parent_external_id: None,
            inactive: false,
            detail: &json!({ "start": "2027-01-01", "end": "2027-01-31", "closed": true }),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let bill = fx.invoice("1001").await;
    assert_eq!(code(fx.post(bill).await.unwrap_err()), "period_closed");
    assert!(fx.fake.calls().is_empty());
    assert_eq!(
        fx.link("bill", bill).await.error_code.as_deref(),
        Some("period_closed")
    );
    fx.db.drop().await;

    let Some(fx) = Fx::open(
        "USD",
        json!({ "expense_account_id": "65" }),
        Some(json!({ "type": "expense", "requires": ["department"] })),
    )
    .await
    else {
        return;
    };
    let bill = fx.invoice("1002").await;
    let err = fx.post(bill).await.unwrap_err();
    assert_eq!(code(err), "missing_field");
    assert!(fx.fake.calls().is_empty());
    fx.db.drop().await;

    let Some(fx) = Fx::usd().await else { return };
    let bill = fx
        .invoice_lines(
            "1003",
            10_000,
            json!([{ "description": "work", "amount_cents": 10000, "account": "999" }]),
        )
        .await;
    assert_eq!(code(fx.post(bill).await.unwrap_err()), "unknown_account");
    assert!(fx.fake.calls().is_empty());
    fx.db.drop().await;

    let Some(fx) = Fx::open("USD", json!({}), None).await else {
        return;
    };
    let bill = fx.invoice("1004").await;
    assert_eq!(code(fx.post(bill).await.unwrap_err()), "missing_field");
    assert!(fx.fake.calls().is_empty());
    fx.db.drop().await;

    let Some(fx) = Fx::open(
        "USD",
        json!({ "expense_account_id": "65" }),
        Some(json!({ "type": "expense" })),
    )
    .await
    else {
        return;
    };
    let mut tx = fx.db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::insert_ledger_record(
        &mut tx,
        DEFAULT_ORG,
        &NewLedgerRecord {
            connection_id: fx.connection,
            kind: "vendor",
            external_id: "ns-1",
            name: "Acme",
            parent_external_id: None,
            inactive: false,
            detail: &json!({ "currency": "EUR" }),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let bill = fx.invoice("1005").await;
    assert_eq!(code(fx.post(bill).await.unwrap_err()), "currency");
    assert!(fx.fake.calls().is_empty());
    fx.db.drop().await;

    let Some(fx) = Fx::open(
        "EUR",
        json!({ "expense_account_id": "65" }),
        Some(json!({ "type": "expense" })),
    )
    .await
    else {
        return;
    };
    let bill = fx.invoice("1006").await;
    assert_eq!(code(fx.post(bill).await.unwrap_err()), "currency");
    assert!(fx.fake.calls().is_empty());
    fx.db.drop().await;
}

#[tokio::test]
async fn duplicate_invoice_and_concurrency() {
    let Some(fx) = Fx::usd().await else { return };
    fx.fake.script_err(LedgerError::new(
        Code::DuplicateInvoice,
        "already has invoice 1001",
    ));
    let bill = fx.invoice("1001").await;
    assert_eq!(code(fx.post(bill).await.unwrap_err()), "duplicate_invoice");
    assert_eq!(fx.fake.calls().len(), 1);
    assert_eq!(fx.link("bill", bill).await.status, "error");
    fx.db.drop().await;

    let Some(fx) = Fx::usd().await else { return };
    fx.fake
        .script_err(LedgerError::new(Code::Concurrency, "busy"));
    let bill = fx.invoice("1002").await;
    let posted = fx.post(bill).await.unwrap();
    assert_eq!(posted["result"]["external_id"], "fake-bill-1");
    assert_eq!(fx.fake.calls().len(), 2);
    assert_eq!(fx.link("bill", bill).await.status, "posted");
    fx.db.drop().await;
}

#[tokio::test]
async fn payment_waits_for_the_bill_and_the_bank_map() {
    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1001").await;
    let (payment, _) = fx.sent_payment(bill, 10_000).await;
    assert_eq!(
        code(fx.post_payment(payment).await.unwrap_err()),
        "bill_not_posted"
    );
    assert!(fx.fake.calls().is_empty());
    fx.db.drop().await;

    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1002").await;
    fx.post(bill).await.unwrap();
    let (payment, _) = fx.sent_payment(bill, 10_000).await;
    assert_eq!(
        code(fx.post_payment(payment).await.unwrap_err()),
        "payment_unmapped"
    );
    assert_eq!(fx.fake.calls().len(), 1);
    fx.db.drop().await;
}

#[tokio::test]
async fn void_unpaid_and_void_paid_payment_first() {
    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1001").await;
    fx.post(bill).await.unwrap();
    let bare = commit(
        &fx.db.state,
        &person("ada"),
        "ledger.void",
        json!({ "bill_id": bill }),
    )
    .await
    .unwrap_err();
    assert!(matches!(bare, CommandError::NeedsConfirmation(_)), "{bare}");
    let voided = fx.void(bill).await.unwrap();
    assert_eq!(voided["result"]["payments_voided"], json!(0));
    assert_eq!(
        fx.fake.calls(),
        vec![
            format!("post_bill {}", payvy_ledger::bill_key(bill)),
            "void_bill fake-bill-1".to_string()
        ]
    );
    assert_eq!(fx.link("bill", bill).await.status, "voided");

    let other = fx.invoice("1001b").await;
    fx.post(other).await.unwrap();
    fx.fake.script_err(LedgerError::new(
        Code::Conflict,
        "the ledger refused the void",
    ));
    assert_eq!(code(fx.void(other).await.unwrap_err()), "conflict");
    let refused = fx.link("bill", other).await;
    assert_eq!(refused.status, "posted");
    assert_eq!(refused.error_code.as_deref(), Some("conflict"));
    fx.db.drop().await;

    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1002").await;
    fx.post(bill).await.unwrap();
    let (payment, bank) = fx.sent_payment(bill, 10_000).await;
    fx.map_bank(bank, "77").await;
    let posted = fx.post_payment(payment).await.unwrap();
    assert_eq!(posted["result"]["external_id"], "fake-payment-1");
    let voided = fx.void(bill).await.unwrap();
    assert_eq!(voided["result"]["payments_voided"], json!(1));
    assert_eq!(
        fx.fake.calls(),
        vec![
            format!("post_bill {}", payvy_ledger::bill_key(bill)),
            format!("post_payment {}", payvy_ledger::payment_key(payment)),
            "void_payment fake-payment-1".to_string(),
            "void_bill fake-bill-1".to_string(),
        ]
    );
    assert_eq!(fx.link("payment", payment).await.status, "voided");
    assert_eq!(fx.link("bill", bill).await.status, "voided");
    fx.db.drop().await;

    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1003").await;
    fx.post(bill).await.unwrap();
    let (payment, bank) = fx.sent_payment(bill, 10_000).await;
    fx.map_bank(bank, "77").await;
    fx.post_payment(payment).await.unwrap();
    fx.fake.script_err(LedgerError::new(
        Code::Conflict,
        "the ledger refused the void",
    ));
    assert_eq!(code(fx.void(bill).await.unwrap_err()), "conflict");
    assert!(
        !fx.fake
            .calls()
            .iter()
            .any(|call| call.starts_with("void_bill"))
    );
    assert_eq!(fx.link("bill", bill).await.status, "posted");
    let payment_link = fx.link("payment", payment).await;
    assert_eq!(payment_link.status, "posted");
    assert_eq!(payment_link.error_code.as_deref(), Some("conflict"));
    fx.db.drop().await;
}

#[tokio::test]
async fn void_reverses_a_returned_payment_and_the_later_one() {
    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1001").await;
    fx.post(bill).await.unwrap();
    let (first, bank_a) = fx.sent_payment(bill, 10_000).await;
    fx.map_bank(bank_a, "77").await;
    fx.post_payment(first).await.unwrap();
    let now = *fx.db.clock.lock().unwrap();
    let mut tx = fx.db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::update_payment(
        &mut tx,
        first,
        &PaymentUpdate {
            status: "returned",
            request_id: None,
            transaction_id: None,
            failure: Some("returned by the bank"),
            link: None,
        },
        now,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let second = fx.sent_again(first, 10_000).await;
    fx.post_payment(second).await.unwrap();
    let preview = commands::run(
        &fx.db.state,
        &person("ada"),
        "ledger.void",
        json!({ "bill_id": bill }),
        Mode::Preview,
    )
    .await
    .unwrap();
    assert_eq!(preview["result"]["payments_to_void"], json!(2));
    let voided = fx.void(bill).await.unwrap();
    assert_eq!(voided["result"]["payments_voided"], json!(2));
    assert_eq!(
        fx.fake.calls(),
        vec![
            format!("post_bill {}", payvy_ledger::bill_key(bill)),
            format!("post_payment {}", payvy_ledger::payment_key(first)),
            format!("post_payment {}", payvy_ledger::payment_key(second)),
            "void_payment fake-payment-1".to_string(),
            "void_payment fake-payment-2".to_string(),
            "void_bill fake-bill-1".to_string(),
        ]
    );
    assert_eq!(fx.link("payment", first).await.status, "voided");
    assert_eq!(fx.link("payment", second).await.status, "voided");
    fx.db.drop().await;
}

#[tokio::test]
async fn a_later_transaction_id_still_replays() {
    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1001").await;
    fx.post(bill).await.unwrap();
    let (payment, bank) = fx.sent_payment(bill, 10_000).await;
    fx.map_bank(bank, "77").await;
    let posted = fx.post_payment(payment).await.unwrap();
    assert_eq!(posted["result"]["replay"], json!(false));
    let now = *fx.db.clock.lock().unwrap();
    let mut tx = fx.db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::update_payment(
        &mut tx,
        payment,
        &PaymentUpdate {
            status: "sent",
            request_id: None,
            transaction_id: Some("txn-later"),
            failure: None,
            link: None,
        },
        now,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let again = fx.post_payment(payment).await.unwrap();
    assert_eq!(again["result"]["replay"], json!(true));
    assert_eq!(
        again["result"]["external_id"],
        posted["result"]["external_id"]
    );
    assert_eq!(fx.fake.calls().len(), 2);
    fx.db.drop().await;
}

#[tokio::test]
async fn a_closed_period_after_posting_still_replays() {
    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1001").await;
    let first = fx.post(bill).await.unwrap();
    close_january(&fx).await;
    let again = fx.post(bill).await.unwrap();
    assert_eq!(again["result"]["replay"], json!(true));
    assert_eq!(
        again["result"]["external_id"],
        first["result"]["external_id"]
    );
    assert_eq!(fx.fake.calls().len(), 1);
    fx.db.drop().await;
}

#[tokio::test]
async fn a_payment_in_a_closed_period_is_not_ready() {
    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1001").await;
    fx.post(bill).await.unwrap();
    let (payment, bank) = fx.sent_payment(bill, 10_000).await;
    fx.map_bank(bank, "77").await;
    close_january(&fx).await;
    let preview = commands::run(
        &fx.db.state,
        &person("ada"),
        "ledger.post_payment",
        json!({ "payment_id": payment }),
        Mode::Preview,
    )
    .await
    .unwrap_err();
    assert_eq!(code(preview), "period_closed");
    assert_eq!(
        code(fx.post_payment(payment).await.unwrap_err()),
        "period_closed"
    );
    assert_eq!(fx.fake.calls().len(), 1);
    fx.db.drop().await;
}

async fn close_january(fx: &Fx) {
    let mut tx = fx.db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::insert_ledger_record(
        &mut tx,
        DEFAULT_ORG,
        &NewLedgerRecord {
            connection_id: fx.connection,
            kind: "period",
            external_id: "2027-01",
            name: "Jan 2027",
            parent_external_id: None,
            inactive: false,
            detail: &json!({ "start": "2027-01-01", "end": "2027-01-31", "closed": true }),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_sealed_token_is_what_the_database_holds() {
    let Some(fx) = Fx::usd().await else { return };
    let logs = Arc::new(Mutex::new(Vec::<u8>::new()));
    let captured = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || LogWriter(captured.clone()))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let bill = fx.invoice("1001").await;
    let posted = fx.post(bill).await.unwrap();
    let result = serde_json::to_string(&posted).unwrap();
    assert!(!result.contains(SECRET), "{result}");
    let mut tx = fx.db.state.store.begin(DEFAULT_ORG).await.unwrap();
    let connection = store::active_ledger_connection(&mut tx)
        .await
        .unwrap()
        .unwrap();
    assert!(!String::from_utf8_lossy(&connection.token_sealed).contains(SECRET));
    let rendered = format!("{connection:?}");
    assert!(
        rendered.contains("token_sealed: \"[sealed]\""),
        "{rendered}"
    );
    assert!(!rendered.contains(SECRET), "{rendered}");
    let events = store::list_events(&mut tx, None, 50).await.unwrap();
    let event_text = serde_json::to_string(&events).unwrap();
    assert!(!event_text.contains(SECRET), "{event_text}");
    drop(tx);
    {
        let held = logs.lock().unwrap();
        let log_text = String::from_utf8_lossy(&held);
        assert!(!log_text.contains(SECRET), "{log_text}");
    }
    drop(_guard);
    fx.db.drop().await;
}

#[tokio::test]
async fn rejected_bills_are_refused_and_paid_bills_post() {
    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1001").await;
    let mut tx = fx.db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::close_bill(&mut tx, bill, State::Rejected, "no")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(fx.post(bill).await, Err(CommandError::Invalid(_))));
    assert!(fx.fake.calls().is_empty());
    fx.db.drop().await;

    let Some(fx) = Fx::usd().await else { return };
    let bill = fx.invoice("1002").await;
    let mut tx = fx.db.state.store.begin(DEFAULT_ORG).await.unwrap();
    store::mark_paid(
        &mut tx,
        bill,
        chrono::NaiveDate::from_ymd_opt(2027, 2, 1).unwrap(),
        "ach",
        Some("txn"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let posted = fx.post(bill).await.unwrap();
    assert_eq!(posted["result"]["external_id"], "fake-bill-1");
    fx.db.drop().await;
}

#[tokio::test]
async fn posting_without_a_ledger_says_so() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let err = commit(
        &db.state,
        &person("ada"),
        "ledger.post_bill",
        json!({ "bill_id": Uuid::nil() }),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CommandError::NotConfigured(_)), "{err}");
    db.drop().await;
}
