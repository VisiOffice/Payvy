//! Bounded, local PDF text/OCR adapters. No shell and no remote OCR service.
use payvy_domain::{documents::Kind, invoice_evidence::SourcePage};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

// Shared across organizations and requests in this API process. Acquire before
// creating scratch files; waiting is included in the 90-second source deadline.
static OCR_JOBS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
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
        .env("OMP_THREAD_LIMIT", "1")
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
    read_limited(bytes, kind, pages, &OCR_JOBS).await
}
async fn read_limited(
    bytes: &[u8],
    kind: Kind,
    pages: &[u32],
    slots: &tokio::sync::Semaphore,
) -> Vec<SourcePage> {
    tokio::time::timeout(Duration::from_secs(90), async {
        let _permit = slots.acquire().await.ok()?;
        collect(bytes, kind, pages).await
    })
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
                let _ = std::fs::remove_file(prefix.with_extension("png"));
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
        // Retain only the input and one rendered page, even on long scans.
        if kind == Kind::Pdf {
            let _ = std::fs::remove_file(&image);
        }
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
    #[tokio::test]
    async fn source_work_waits_for_capacity_and_canceled_waiters_do_not_take_it() {
        let slots = tokio::sync::Semaphore::new(1);
        let occupied = slots.acquire().await.unwrap();
        let data = include_bytes!("../../../fixtures/invoices/king-environmental-189727.pdf");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                read_limited(data, Kind::Pdf, &[1], &slots)
            )
            .await
            .is_err()
        );
        drop(occupied);
        let pages = tokio::time::timeout(
            Duration::from_secs(5),
            read_limited(data, Kind::Pdf, &[1], &slots),
        )
        .await
        .unwrap();
        assert_eq!(pages[0].method, "native_text");
        assert_eq!(slots.available_permits(), 1);
    }
}
