//! How many pages a document has, read from the file. Images are one page.
//! A PDF is opened with `lopdf`, off the async runtime and behind a panic
//! guard: a malformed or hostile file yields "can't tell", never a crash.

use payvy_domain::documents::Kind;

/// The page count, or None when the file can't be read as a PDF.
pub async fn count(bytes: Vec<u8>, kind: Kind) -> Option<u32> {
    if kind != Kind::Pdf {
        return Some(1);
    }
    // A panic inside the parser ends this blocking task, not the server:
    // the JoinError comes back as "can't tell".
    tokio::task::spawn_blocking(move || count_pdf(&bytes))
        .await
        .ok()
        .flatten()
}

/// More pages than any invoice has. A file claiming more is treated as
/// unreadable: the count comes from the file itself, and every page becomes
/// an entry in the arrangement, so a hostile /Count (anyone can email the
/// inbox) must not size an allocation.
pub const MAX_PAGES: u32 = 2_000;

/// The page tree's own count first (what pdf.js shows too), then a full load
/// that walks the tree, which also opens files encrypted with an empty
/// password.
fn count_pdf(bytes: &[u8]) -> Option<u32> {
    let from_metadata = lopdf::Document::load_metadata_mem(bytes)
        .ok()
        .map(|m| m.page_count);
    if let Some(n) = from_metadata.filter(|n| *n > 0) {
        return (n <= MAX_PAGES).then_some(n);
    }
    let doc = lopdf::Document::load_mem(bytes).ok()?;
    let n = doc.get_pages().len();
    u32::try_from(n)
        .ok()
        .filter(|n| (1..=MAX_PAGES).contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF with `n` blank pages, built by hand.
    fn blank_pdf(n: usize) -> Vec<u8> {
        pdf_claiming(n, n)
    }

    /// `n` real pages under a page tree whose /Count claims `claimed`.
    fn pdf_claiming(n: usize, claimed: usize) -> Vec<u8> {
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", i + 3)).collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {claimed} >>",
                kids.join(" ")
            ),
        ];
        for _ in 0..n {
            objects.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into());
        }
        let mut out = String::from("%PDF-1.4\n");
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

    #[tokio::test]
    async fn counts_pages_and_survives_bad_files() {
        assert_eq!(count(blank_pdf(1), Kind::Pdf).await, Some(1));
        assert_eq!(count(blank_pdf(4), Kind::Pdf).await, Some(4));
        assert_eq!(
            count(b"\xFF\xD8\xFF\xE0".to_vec(), Kind::Jpeg).await,
            Some(1)
        );
        // Not really a PDF past the header, cut short, or garbage.
        assert_eq!(
            count(b"%PDF-1.4\n% nothing\n%%EOF\n".to_vec(), Kind::Pdf).await,
            None
        );
        let mut cut = blank_pdf(3);
        cut.truncate(cut.len() / 2);
        assert!(count(cut, Kind::Pdf).await.is_none_or(|n| n <= 3));
        let mut junk = b"%PDF-1.7\n".to_vec();
        junk.extend((0..20_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8));
        assert!(count(junk, Kind::Pdf).await.is_none_or(|n| n > 0));
    }

    #[tokio::test]
    async fn a_file_claiming_too_many_pages_cannot_be_arranged() {
        assert_eq!(count(pdf_claiming(2, 2), Kind::Pdf).await, Some(2));
        assert_eq!(count(pdf_claiming(2, 2_000_000_000), Kind::Pdf).await, None);
        assert_eq!(
            count(pdf_claiming(2, MAX_PAGES as usize + 1), Kind::Pdf).await,
            None
        );
    }
}
