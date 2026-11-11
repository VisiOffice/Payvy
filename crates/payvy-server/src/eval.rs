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
        if expected["schema_version"] == 2 {
            let pages: Vec<u32> = serde_json::from_value(expected["pages"].clone())?;
            let mut checks = Vec::new();
            compare_fields("", &expected["expected"], &Value::Null, &mut checks);
            let count = checks.len() + 1; // complete, exact selected-page coverage
            score.total += count;
            match extractor.read_pages_v2(&bytes, kind, &pages).await {
                Err(e) => {
                    score.failed_documents += 1;
                    println!("  {name}: 0/{count} failed: {e}");
                }
                Ok(out) => {
                    checks.clear();
                    compare_fields("", &expected["expected"], &json!(out.read), &mut checks);
                    checks.push((
                        "coverage".into(),
                        out.read
                            .coverage
                            .as_ref()
                            .is_some_and(|c| c.complete && c.pages_read == pages),
                    ));
                    let right = checks.iter().filter(|(_, ok)| *ok).count();
                    score.right += right;
                    let sources = crate::source_text::read(&bytes, kind, &pages).await;
                    let mut located = 0;
                    let mut checked = 0;
                    for row in &out.read.lines {
                        let details = row.details.clone().unwrap_or_default();
                        let mut fields = vec![
                            ("description", Some(row.description.as_str())),
                            ("amount", Some(row.amount.as_str())),
                        ];
                        fields.extend(details.fields());
                        for (field, value) in fields {
                            if let Some(value) = value {
                                checked += 1;
                                let evidence = payvy_domain::invoice_evidence::ground(
                                    field,
                                    value,
                                    &row.description,
                                    &details,
                                    &sources,
                                );
                                located +=
                                    usize::from(evidence.iter().all(|g| g.status == "located"));
                            }
                        }
                    }
                    println!(
                        "  {name}: {right}/{count} exact fields/row counts/coverage; {located}/{checked} fields have located evidence (not correctness)"
                    );
                    for (field, ok) in checks {
                        if !ok {
                            println!("    mismatch: {field}");
                        }
                    }
                }
            }
            continue;
        }
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

/// Expected nulls are scored too: inventing an absent value is an error.
/// Arrays score row count as well as every row's fields, preserving repeated rows.
fn compare_fields(path: &str, want: &Value, got: &Value, checks: &mut Vec<(String, bool)>) {
    match want {
        Value::Object(map) => {
            for (key, value) in map {
                compare_fields(&format!("{path}/{key}"), value, &got[key], checks);
            }
        }
        Value::Array(rows) => {
            checks.push((
                format!("{path}/count"),
                got.as_array().is_some_and(|r| r.len() == rows.len()),
            ));
            for (i, row) in rows.iter().enumerate() {
                compare_fields(&format!("{path}/{i}"), row, &got[i], checks);
            }
        }
        _ => checks.push((path.into(), want == got)),
    }
}
#[cfg(test)]
mod v2_tests {
    use super::*;
    #[test]
    fn detects_missing_repeated_rows_and_invented_absent_values() {
        let want =
            json!({"lines":[{"quantity":null,"amount":"0.00"},{"quantity":null,"amount":"0.00"}]});
        let got = json!({"lines":[{"quantity":"0","amount":"0.00"}]});
        let mut checks = Vec::new();
        compare_fields("", &want, &got, &mut checks);
        assert!(checks.contains(&("/lines/count".into(), false)));
        assert!(checks.contains(&("/lines/0/quantity".into(), false)));
        assert!(checks.contains(&("/lines/1/amount".into(), false)));
    }
}
