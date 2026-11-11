//! Bounded, local PDF text/OCR adapters. No shell and no remote OCR service.
use payvy_domain::{documents::Kind, invoice_evidence::SourcePage};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

const TEXT_LIMIT: usize = 128 * 1024;
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn scratch() -> std::io::Result<Scratch> {
    let p = std::env::temp_dir().join(format!("payvy-ocr-{}", uuid::Uuid::new_v4()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&p)?;
    Ok(Scratch(p))
}
async fn output(program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?.take((TEXT_LIMIT + 1) as u64);
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        stdout.read_to_end(&mut bytes).await.ok()?;
        if bytes.len() > TEXT_LIMIT {
            return None;
        }
        child.wait().await.ok()?.success().then_some(())
    })
    .await
    .ok()
    .flatten();
    if result.is_none() {
        let _ = child.kill().await;
        return None;
    }
    String::from_utf8(bytes).ok()
}
fn path(p: &Path) -> &str {
    p.to_str().unwrap_or("")
}
fn missing(pages: &[u32]) -> Vec<SourcePage> {
    pages
        .iter()
        .map(|&page| SourcePage {
            page,
            method: "unavailable".into(),
            text: None,
        })
        .collect()
}

pub async fn read(bytes: &[u8], kind: Kind, pages: &[u32]) -> Vec<SourcePage> {
    if pages.is_empty() || pages.len() > 24 {
        return missing(pages);
    }
    tokio::time::timeout(Duration::from_secs(90), collect(bytes, kind, pages))
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| missing(pages))
}
async fn collect(bytes: &[u8], kind: Kind, pages: &[u32]) -> Option<Vec<SourcePage>> {
    let dir = scratch().ok()?;
    let input = dir.0.join(if kind == Kind::Pdf {
        "source.pdf"
    } else {
        "source.image"
    });
    std::fs::write(&input, bytes).ok()?;
    let mut found = Vec::new();
    for &page in pages {
        let p = page.to_string();
        let native = if kind == Kind::Pdf {
            output(
                "pdftotext",
                &["-f", &p, "-l", &p, "-layout", "-nopgbrk", path(&input), "-"],
            )
            .await
        } else {
            None
        };
        if let Some(text) = native.filter(|s| s.trim().chars().count() >= 8) {
            found.push(SourcePage {
                page,
                method: "native_text".into(),
                text: Some(text),
            });
            continue;
        }
        let image = if kind == Kind::Pdf {
            let prefix = dir.0.join(format!("page-{page}"));
            let ok = output(
                "pdftoppm",
                &[
                    "-f",
                    &p,
                    "-l",
                    &p,
                    "-singlefile",
                    "-scale-to",
                    "1800",
                    "-png",
                    path(&input),
                    path(&prefix),
                ],
            )
            .await;
            if ok.is_none() {
                found.extend(missing(&[page]));
                continue;
            }
            prefix.with_extension("png")
        } else {
            input.clone()
        };
        let text = output(
            "tesseract",
            &[path(&image), "stdout", "-l", "eng", "--psm", "3"],
        )
        .await
        .filter(|s| !s.trim().is_empty());
        found.push(SourcePage {
            page,
            method: if text.is_some() { "ocr" } else { "unavailable" }.into(),
            text,
        });
    }
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn missing_tools_or_bad_documents_are_uncertainty() {
        assert!(output("/does-not-exist/payvy-ocr", &[]).await.is_none());
        let pages = read(b"%PDF-1.4\n%%EOF", Kind::Pdf, &[2]).await;
        assert_eq!(pages[0].page, 2);
        assert_eq!(pages[0].method, "unavailable");
    }
    #[tokio::test]
    async fn native_fixture_is_located_without_a_model() {
        let data = include_bytes!("../../../fixtures/invoices/king-environmental-189727.pdf");
        let pages = read(data, Kind::Pdf, &[1]).await;
        assert_eq!(
            pages[0].method, "native_text",
            "Install poppler-utils for extraction tests"
        );
        assert!(pages[0].text.as_ref().unwrap().contains("189727"));
    }
    #[tokio::test]
    async fn scanned_image_uses_ocr_and_keeps_page_identity() {
        let bytes = include_bytes!("../../../fixtures/invoices-v2/scanned.png");
        let pages = read(bytes, Kind::Png, &[1]).await;
        assert_eq!(pages[0].page, 1);
        assert_eq!(
            pages[0].method, "ocr",
            "Install tesseract-ocr with English language data for extraction tests"
        );
        assert!(pages[0].text.as_ref().unwrap().contains("Disposal"));
    }
}
