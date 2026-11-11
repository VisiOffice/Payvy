//! The actual Python importer against the real HTTP router, Postgres/RLS and
//! S3. Only model responses are faked; no legacy or production data is used.
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, Mutex},
};

use axum::{Json, extract::State, routing::post};
use payvy_extract::{Config, Extractor, Provider};
use serde_json::{Value, json};

mod common;
use common::{ADMIN_KEY, TestDb, agent, commit};

async fn model(State(seen): State<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>) -> Json<Value> {
    seen.lock().unwrap().push(body);
    Json(
        json!({"model":"test-model", "stop_reason":"end_turn", "content":[{
            "type":"text", "text":json!({
                "kind":"invoice", "vendor_name":"King Environmental LLC", "vendor_email":null,
                "vendor_address":null, "account_number":null, "invoice_number":"189727",
                "invoice_date":"2027-01-10", "due_date":"2027-02-09", "terms":"Net 30",
                "currency":"USD", "po_number":null, "subtotal":"425.00", "tax":"34.00",
                "total":"459.00", "lines":[{"description":"Waste service", "amount":"425.00"}],
                "notes":null
            }).to_string()
        }]}),
    )
}

async fn python(root: &Path, script: &str, args: Vec<String>) -> Output {
    let script = root.join("scripts/legacy_import").join(script);
    tokio::task::spawn_blocking(move || {
        Command::new("python3")
            .arg(script)
            .args(args)
            .env("PAYVY_IMPORT_API_KEY", ADMIN_KEY)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}
fn success(output: Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
struct Bundle(PathBuf);
impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn pilot_upload_resume_and_lost_journal_never_create_bills() {
    let Some(mut db) = TestDb::with_storage().await else {
        return;
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bundle =
        Bundle(std::env::temp_dir().join(format!("payvy-import-{}", uuid::Uuid::new_v4())));
    success(
        python(
            &root,
            "sample.py",
            vec!["--output".into(), bundle.0.display().to_string()],
        )
        .await,
    );
    let manifest = bundle.0.join("manifest.json");
    let mut contents: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    // Legacy reference metadata must never be sent to the model as its answer.
    contents["entries"][0]["reference"]["vendor_name"] = json!("DO NOT SEND LEGACY ANSWER");
    std::fs::write(&manifest, serde_json::to_vec_pretty(&contents).unwrap()).unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}", listener.local_addr().unwrap());
    let model_app = axum::Router::new()
        .route("/v1/messages", post(model))
        .with_state(seen.clone());
    let model_task = tokio::spawn(async move { axum::serve(listener, model_app).await.unwrap() });
    db.state = db.state.clone().with_extractor(
        Extractor::new(Config {
            provider: Provider::Anthropic,
            base_url: model_url,
            api_key: Some("test-only".into()),
            model: "test-model".into(),
        })
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = payvy_server::router(db.state.clone());
    let server_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let args = vec![manifest.display().to_string(), "--limit".into(), "1".into()];
    success(python(&root, "import.py", args.clone()).await);
    assert!(!bundle.0.join("import-state.json").exists());
    assert!(seen.lock().unwrap().is_empty());
    let ap = agent("import-test");
    let empty = commit(&db.state, &ap, "document.list", json!({}))
        .await
        .unwrap();
    assert_eq!(empty["result"]["documents"], json!([]));

    let apply = [
        args,
        vec!["--api".into(), url, "--apply".into(), "--extract".into()],
    ]
    .concat();
    success(python(&root, "import.py", apply.clone()).await);
    success(python(&root, "import.py", apply.clone()).await);
    // Simulate lost local state after the server has committed an upload.
    std::fs::remove_file(bundle.0.join("import-state.json")).unwrap();
    success(python(&root, "import.py", apply).await);
    let journal = std::fs::read_to_string(bundle.0.join("import-state.json")).unwrap();
    assert!(!journal.contains(ADMIN_KEY));
    let journal: Value = serde_json::from_str(&journal).unwrap();
    let source_id = contents["entries"][0]["source_id"].as_str().unwrap();
    assert_eq!(journal["entries"][source_id]["status"], "duplicate");
    assert_eq!(journal["entries"][source_id]["extraction"], "already_read");
    let docs = commit(&db.state, &ap, "document.list", json!({}))
        .await
        .unwrap();
    let docs = docs["result"]["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["status"], "inbox");
    let detail = commit(
        &db.state,
        &ap,
        "document.get",
        json!({"document_id":docs[0]["id"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        detail["result"]["extractions"][0]["extraction"]["draft"]["invoice_number"],
        "189727"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "resuming does not spend model credits again"
    );
    assert!(
        !serde_json::to_string(&*seen.lock().unwrap())
            .unwrap()
            .contains("DO NOT SEND LEGACY ANSWER")
    );
    for (command, key) in [
        ("bill.list", "bills"),
        ("vendor.list", "vendors"),
        ("payment.list", "payments"),
    ] {
        let result = commit(&db.state, &ap, command, json!({})).await.unwrap();
        assert_eq!(result["result"][key], json!([]), "{command}");
    }
    let other = db.add_org().await;
    let other_docs = commit(
        &db.state,
        &payvy_server::Actor::agent(other, "other"),
        "document.list",
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(other_docs["result"]["documents"], json!([]));
    server_task.abort();
    model_task.abort();
    db.drop().await;
}
