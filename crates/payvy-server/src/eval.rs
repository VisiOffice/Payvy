//! Scores an extraction model against sample documents with known answers
//! (`fixtures/invoices`), so a self-hoster can see how a model does before
//! trusting it, and the README can say how each one does instead of
//! claiming they're all equal. `payvy extract-eval fixtures/invoices`.

use std::path::Path;

use payvy_domain::documents;
use payvy_domain::extraction::{self, name_key};
use payvy_extract::Extractor;
use serde_json::{Value, json};

pub struct Score {
    pub right: usize,
    pub total: usize,
    pub failed_documents: usize,
}

/// Runs every `<name>.expected.json` in `dir` against `<name>.pdf` (or .png,
/// .jpg) and prints one line per document.
pub async fn run(extractor: &Extractor, dir: &Path) -> anyhow::Result<Score> {
    let mut cases: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".expected.json"))
        .collect();
    cases.sort();
    anyhow::ensure!(
        !cases.is_empty(),
        "no *.expected.json files in {}",
        dir.display()
    );
    let mut score = Score {
        right: 0,
        total: 0,
        failed_documents: 0,
    };
    println!("{} via {}", extractor.model(), extractor.provider_label());
    for case in cases {
        let name = case
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .trim_end_matches(".expected.json")
            .to_owned();
        let expected: Value = serde_json::from_str(&std::fs::read_to_string(&case)?)?;
        let file = ["pdf", "png", "jpg"]
            .iter()
            .map(|ext| dir.join(format!("{name}.{ext}")))
            .find(|p| p.exists())
            .ok_or_else(|| anyhow::anyhow!("{name}: no document next to its expected answer"))?;
        let bytes = std::fs::read(&file)?;
        let kind = documents::check(&bytes)?;
        let fields = expected.as_object().map(|o| o.len()).unwrap_or(0);
        score.total += fields;
        let out = match extractor.read(&bytes, kind).await {
            Ok(out) => out,
            Err(e) => {
                score.failed_documents += 1;
                println!("  {name}: 0/{fields}  failed: {e}");
                continue;
            }
        };
        let (draft, checks) = extraction::interpret(&out.read);
        let draft = json!(draft);
        let mut wrong = Vec::new();
        for (field, want) in expected.as_object().into_iter().flatten() {
            let got = match field.as_str() {
                "line_count" => json!(draft["lines"].as_array().map_or(0, Vec::len)),
                "vendor_name" => json!(draft[field].as_str().map(name_key)),
                _ => draft[field].clone(),
            };
            let want = match field.as_str() {
                "vendor_name" => json!(want.as_str().map(name_key)),
                _ => want.clone(),
            };
            if got == want {
                score.right += 1;
            } else {
                wrong.push(format!("{field}: wanted {want}, got {got}"));
            }
        }
        let failed_checks: Vec<&str> = checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| c.check.as_str())
            .collect();
        println!(
            "  {name}: {}/{fields}{}{}",
            fields - wrong.len(),
            if wrong.is_empty() {
                String::new()
            } else {
                format!("  wrong: {}", wrong.join("; "))
            },
            if failed_checks.is_empty() {
                String::new()
            } else {
                format!("  failed checks: {}", failed_checks.join(", "))
            },
        );
    }
    println!("{}/{} fields right", score.right, score.total);
    Ok(score)
}
