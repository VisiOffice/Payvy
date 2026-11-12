//! "Create N bills" on a file of several invoices, against a real database
//! (with row-level security) and a real S3-compatible store: the command
//! splits the file at once and is safe to retry, and the batch worker reads
//! each part and makes its bill as the person who asked, stacking a copy of
//! a bill already here and leaving anything else for a person.

use axum::Json;
use axum::routing::post;
use base64::Engine;
use payvy_extract::{Config, Extractor, Provider};
use payvy_server::Actor;
use payvy_server::commands::CommandError;
use payvy_store::DEFAULT_ORG;
use serde_json::{Value, json};

mod common;
use common::{TestDb, commit, person, vendor};

/// A PDF with `n` blank pages, different per call so uploads don't collide.
fn pdf(n: usize) -> Vec<u8> {
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", i + 3)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
    ];
    for _ in 0..n {
        objects.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into());
    }
    let mut out = format!("%PDF-1.4\n% {}\n", uuid::Uuid::new_v4());
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out += &format!("{} 0 obj\n{o}\nendobj\n", i + 1);
    }
    let xref = out.len();
    out += &format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for o in offsets {
        out += &format!("{o:010} 00000 n \n");
    }
    out += &format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.into_bytes()
}

fn invoice(number: Option<&str>, total: &str, invoices: Value) -> Value {
    json!({
        "kind": if number.is_some() { "invoice" } else { "other" },
        "vendor_name": "Saguaro Storage Co", "vendor_email": null,
        "vendor_address": null, "account_number": null, "invoice_number": number,
        "invoice_date": "2027-01-10", "due_date": null, "terms": null, "currency": "USD",
        "po_number": null, "subtotal": null, "tax": null, "total": total,
        "lines": [{ "description": "Storage", "amount": total }],
        "notes": null, "invoices": invoices
    })
}

/// Four pages: SS-1 on pages 1–2, SS-2 on page 3, a slip on page 4. Told
/// to read some pages, it reads the invoice on them.
async fn with_model(db: &mut TestDb) {
    let app = axum::Router::new().route(
        "/v1/messages",
        post(|Json(body): Json<Value>| async move {
            let prompt = body["messages"][0]["content"][1]["text"]
                .as_str()
                .unwrap_or_default();
            let answer = if prompt.contains("Read only pages 1, 2 of") {
                invoice(Some("SS-1"), "300.00", json!([]))
            } else if prompt.contains("Read only page 3 of") {
                invoice(Some("SS-2"), "120.00", json!([]))
            } else if prompt.contains("Read only page 4 of") {
                invoice(None, "0.00", json!([]))
            } else {
                invoice(
                    Some("SS-1"),
                    "300.00",
                    json!([
                        { "pages": [1, 2], "invoice_number": "SS-1", "total": "300.00" },
                        { "pages": [3], "invoice_number": "SS-2", "total": "120.00" }
                    ]),
                )
            };
            Json(
                json!({ "model": "claude-opus-5-5", "stop_reason": "end_turn",
                         "content": [{ "type": "text", "text": answer.to_string() }] }),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let extractor = Extractor::new(Config {
        provider: Provider::Anthropic,
        base_url: format!("http://{addr}"),
        api_key: Some("test-model-key".into()),
        model: "claude-opus-5-5".into(),
    })
    .unwrap();
    db.state = db.state.clone().with_extractor(extractor);
}

async fn get(db: &TestDb, who: &Actor, id: &str) -> Value {
    commit(&db.state, who, "document.get", json!({ "document_id": id }))
        .await
        .unwrap()["result"]
        .clone()
}

/// An AP clerk, as a signed-in person is: their role applies to the bills
/// the worker makes for them.
async fn clerk(db: &TestDb) -> Actor {
    commit(
        &db.state,
        &person("operator"),
        "person.add",
        json!({ "name": "Cleo", "actor": "cleo", "email": "cleo@example.com", "roles": ["ap_clerk"] }),
    )
    .await
    .unwrap();
    Actor::person(DEFAULT_ORG, "cleo")
}

#[tokio::test]
async fn create_bills_splits_once_and_the_worker_bills_each_part() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    with_model(&mut db).await;
    let cleo = clerk(&db).await;
    let v = vendor(&db.state, &cleo, "Saguaro Storage Co").await;
    // SS-2 is already a bill: the copy in the file stacks on it.
    commit(
        &db.state,
        &cleo,
        "bill.create",
        json!({ "vendor_id": v, "invoice_number": "SS-2", "invoice_date": "2027-01-10", "amount_cents": 12000 }),
    )
    .await
    .unwrap();
    let id = commit(
        &db.state,
        &cleo,
        "document.upload",
        json!({ "filename": "saguaro.pdf",
                "content_base64": base64::engine::general_purpose::STANDARD.encode(pdf(5)) }),
    )
    .await
    .unwrap()["result"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A person's reading suggests the invoices; it doesn't split.
    commit(
        &db.state,
        &cleo,
        "document.extract",
        json!({ "document_id": id }),
    )
    .await
    .unwrap();

    // Groups and leftover must account for every visible page once.
    let request = uuid::Uuid::new_v4();
    let ask = |groups: Value, leftover: Value| {
        json!({ "document_id": id, "expected_version": 0, "request_id": request,
                "groups": groups, "leftover": leftover })
    };
    let wrong = commit(
        &db.state,
        &cleo,
        "document.create_bills",
        ask(json!([[1, 2], [3]]), json!([4])),
    )
    .await;
    assert!(matches!(wrong, Err(CommandError::Invalid(_))), "{wrong:?}");

    // Three invoices (the person made page 4 its own), page 5 left over.
    let input = ask(json!([[1, 2], [3], [4]]), json!([5]));
    let made = commit(&db.state, &cleo, "document.create_bills", input.clone())
        .await
        .unwrap()["result"]
        .clone();
    assert_eq!(made["parts"].as_array().unwrap().len(), 3);
    assert_eq!(made["counts"]["reading"], 3);
    assert_eq!(made["parts"][0]["invoice_number"], "SS-1");
    assert_eq!(made["parts"][0]["total_cents"], 30000);
    assert!(
        made["parts"][2]["invoice_number"].is_null(),
        "page 4 alone wasn't an invoice the reader found"
    );

    // A retry (or double click) returns the same batch: nothing split twice.
    let again = commit(&db.state, &cleo, "document.create_bills", input)
        .await
        .unwrap()["result"]
        .clone();
    assert_eq!(again["id"], made["id"]);
    assert_eq!(again["repeated"], true);
    let source = get(&db, &cleo, &id).await;
    assert_eq!(source["split_into"].as_array().unwrap().len(), 3);
    assert_eq!(source["document"]["status"], "inbox", "page 5 stays");
    let left: Vec<u64> = source["pages"]["arrangement"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["page"].as_u64().unwrap())
        .collect();
    assert_eq!(left, [5]);
    assert_eq!(source["bill_batch"]["id"], made["id"]);

    // Another organization sees no batch at all.
    let mut tx = db.state.store.begin(uuid::Uuid::new_v4()).await.unwrap();
    let doc_id: uuid::Uuid = id.parse().unwrap();
    assert!(
        payvy_store::latest_bill_batch(&mut tx, doc_id)
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();

    // One pass of the worker reads and decides every part.
    assert_eq!(
        payvy_server::bill_batch_worker::tick(&db.state)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        payvy_server::bill_batch_worker::tick(&db.state)
            .await
            .unwrap(),
        0
    );
    let batch = get(&db, &cleo, &id).await["bill_batch"].clone();
    assert_eq!(batch["done"], true);
    let parts = batch["parts"].as_array().unwrap();
    assert_eq!(parts[0]["state"], "created", "{parts:#?}");
    assert_eq!(parts[1]["state"], "stacked", "{parts:#?}");
    assert!(parts[1]["note"].as_str().unwrap().contains("resend"));
    assert_eq!(parts[2]["state"], "needs_you", "{parts:#?}");
    assert!(parts[2]["note"].as_str().unwrap().starts_with("Needs you"));

    // The bill is the person's, filed with its part; the stacked copy is on the old bill.
    let bill = commit(
        &db.state,
        &cleo,
        "bill.get",
        json!({ "bill_id": parts[0]["bill_id"] }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(bill["bill"]["amount_cents"], 30000);
    assert_eq!(bill["bill"]["created_by"], "cleo");
    let first = get(&db, &cleo, parts[0]["document_id"].as_str().unwrap()).await;
    assert_eq!(first["document"]["status"], "filed");
    let slip = get(&db, &cleo, parts[2]["document_id"].as_str().unwrap()).await;
    assert_eq!(slip["document"]["status"], "inbox", "waits in To review");
}

#[tokio::test]
async fn a_part_a_person_handles_first_is_withdrawn() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    with_model(&mut db).await;
    let cleo = clerk(&db).await;
    vendor(&db.state, &cleo, "Saguaro Storage Co").await;
    let id = commit(
        &db.state,
        &cleo,
        "document.upload",
        json!({ "filename": "saguaro.pdf",
                "content_base64": base64::engine::general_purpose::STANDARD.encode(pdf(3)) }),
    )
    .await
    .unwrap()["result"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let made = commit(
        &db.state,
        &cleo,
        "document.create_bills",
        json!({ "document_id": id, "expected_version": 0, "request_id": uuid::Uuid::new_v4(),
                "groups": [[1, 2], [3]] }),
    )
    .await
    .unwrap()["result"]
        .clone();
    // Every page went to a part: the file left To review.
    assert_eq!(get(&db, &cleo, &id).await["document"]["status"], "split");
    let second = made["parts"][1]["document_id"].as_str().unwrap().to_owned();
    commit(
        &db.state,
        &cleo,
        "document.trash",
        json!({ "document_id": second, "reason": "duplicate scan" }),
    )
    .await
    .unwrap();

    payvy_server::bill_batch_worker::tick(&db.state)
        .await
        .unwrap();
    let batch = get(&db, &cleo, &id).await["bill_batch"].clone();
    assert_eq!(batch["parts"][0]["state"], "created");
    assert_eq!(batch["parts"][1]["state"], "withdrawn");
    assert_eq!(batch["counts"]["created"], 1);
}

/// A real model takes tens of seconds a part. A worker holds only the part
/// it is reading, so a second worker (another process, or a restart) takes
/// the next part rather than one the first already holds: at the size of a
/// real file, 38 invoices with the first running three pages, every part is
/// read once and billed once, however the two workers interleave.
#[tokio::test]
async fn a_slow_model_and_two_workers_read_each_part_once() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    // Each read takes a minute of the test's clock.
    let reads: std::sync::Arc<std::sync::Mutex<Vec<u32>>> = Default::default();
    let (seen, clock) = (reads.clone(), db.clock.clone());
    let app = axum::Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let (seen, clock) = (seen.clone(), clock.clone());
            async move {
                let prompt = body["messages"][0]["content"][1]["text"]
                    .as_str()
                    .unwrap_or_default();
                let page: u32 = prompt
                    .split_once("Read only page")
                    .map(|(_, rest)| {
                        rest.trim_start_matches('s')
                            .trim_start()
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect::<String>()
                    })
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                *clock.lock().unwrap() += chrono::Duration::seconds(60);
                seen.lock().unwrap().push(page);
                let n = if page <= 3 { 1 } else { page - 2 };
                let mut read = invoice(
                    Some(&format!("CF-{}", 1000 + n)),
                    &format!("{}.00", 100 + n),
                    json!([]),
                );
                read["vendor_name"] = json!("Cholla Freight Lines");
                Json(
                    json!({ "model": "claude-opus-5-5", "stop_reason": "end_turn",
                             "content": [{ "type": "text", "text": read.to_string() }] }),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    db.state = db.state.clone().with_extractor(
        Extractor::new(Config {
            provider: Provider::Anthropic,
            base_url: format!("http://{addr}"),
            api_key: Some("test-model-key".into()),
            model: "claude-opus-5-5".into(),
        })
        .unwrap(),
    );
    let cleo = clerk(&db).await;
    vendor(&db.state, &cleo, "Cholla Freight Lines").await;
    let id = commit(
        &db.state,
        &cleo,
        "document.upload",
        json!({ "filename": "cholla.pdf",
                "content_base64": base64::engine::general_purpose::STANDARD.encode(pdf(40)) }),
    )
    .await
    .unwrap()["result"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut groups = vec![vec![1, 2, 3]];
    groups.extend((4..=40).map(|p| vec![p]));
    let made = commit(
        &db.state,
        &cleo,
        "document.create_bills",
        json!({ "document_id": id, "expected_version": 0, "request_id": uuid::Uuid::new_v4(),
                "groups": groups }),
    )
    .await
    .unwrap()["result"]
        .clone();
    assert_eq!(made["parts"].as_array().unwrap().len(), 38);

    // One worker starts; a second joins after twelve minutes of reading,
    // when the first's lease on anything it claimed at the start has run out.
    let first = tokio::spawn({
        let state = db.state.clone();
        async move { payvy_server::bill_batch_worker::tick(&state).await.unwrap() }
    });
    while reads.lock().unwrap().len() < 12 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let second = payvy_server::bill_batch_worker::tick(&db.state)
        .await
        .unwrap();
    let first = first.await.unwrap();
    // Whatever is left (more than one pass's worth) is taken on later passes.
    let mut rest = 0;
    for _ in 0..5 {
        *db.clock.lock().unwrap() += chrono::Duration::minutes(1);
        rest += payvy_server::bill_batch_worker::tick(&db.state)
            .await
            .unwrap();
    }
    assert_eq!(first + second + rest, 38, "each part looked at once");

    let mut read = reads.lock().unwrap().clone();
    read.sort_unstable();
    let mut want = vec![1];
    want.extend(4..=40);
    assert_eq!(read, want, "each part read once");
    let batch = get(&db, &cleo, &id).await["bill_batch"].clone();
    assert_eq!(batch["done"], true);
    assert_eq!(
        batch["counts"],
        json!({ "reading": 0, "created": 38, "stacked": 0, "needs_you": 0, "withdrawn": 0 }),
        "{:#}",
        batch["parts"]
    );
}
