//! What a document may be. A document is the original file a bill came
//! from: a PDF, a scan, a phone photo. Its type is read from its bytes, not
//! from what the sender claims, and it is stored under its content hash so
//! the same file arriving twice is caught.

use serde::{Deserialize, Serialize};

/// Large enough for a long scanned statement; small enough to keep one
/// request reasonable.
pub const MAX_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Pdf,
    Png,
    Jpeg,
    Gif,
    Webp,
    Tiff,
}

impl Kind {
    /// From the file's first bytes. None for anything else (Office files,
    /// HEIC and archives aren't accepted yet).
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        Some(match bytes {
            [b'%', b'P', b'D', b'F', b'-', ..] => Kind::Pdf,
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => Kind::Png,
            [0xFF, 0xD8, 0xFF, ..] => Kind::Jpeg,
            [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => Kind::Gif,
            [
                b'R',
                b'I',
                b'F',
                b'F',
                _,
                _,
                _,
                _,
                b'W',
                b'E',
                b'B',
                b'P',
                ..,
            ] => Kind::Webp,
            [b'I', b'I', 0x2A, 0x00, ..] | [b'M', b'M', 0x00, 0x2A, ..] => Kind::Tiff,
            _ => return None,
        })
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Kind::Pdf => "application/pdf",
            Kind::Png => "image/png",
            Kind::Jpeg => "image/jpeg",
            Kind::Gif => "image/gif",
            Kind::Webp => "image/webp",
            Kind::Tiff => "image/tiff",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Kind::Pdf => "pdf",
            Kind::Png => "png",
            Kind::Jpeg => "jpg",
            Kind::Gif => "gif",
            Kind::Webp => "webp",
            Kind::Tiff => "tif",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DocumentError {
    #[error("the file is empty")]
    Empty,
    #[error("the file is {0} bytes; the limit is 25 MB")]
    TooLarge(usize),
    #[error("not a PDF, PNG, JPEG, GIF, WebP or TIFF file")]
    UnsupportedType,
}

pub fn check(bytes: &[u8]) -> Result<Kind, DocumentError> {
    if bytes.is_empty() {
        return Err(DocumentError::Empty);
    }
    if bytes.len() > MAX_BYTES {
        return Err(DocumentError::TooLarge(bytes.len()));
    }
    Kind::sniff(bytes).ok_or(DocumentError::UnsupportedType)
}

/// Where a document lives in the bucket. Content-addressed per organization,
/// so a retried upload writes the same object.
pub fn storage_key(org_id: &str, sha256_hex: &str, kind: Kind) -> String {
    format!("orgs/{org_id}/documents/{sha256_hex}.{}", kind.extension())
}

/// A filename safe to show and to put in a download header: no path, no
/// control characters, at most 200 characters.
pub fn clean_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "document".into()
    } else {
        cleaned.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_comes_from_the_bytes() {
        assert_eq!(check(b"%PDF-1.7\n..."), Ok(Kind::Pdf));
        assert_eq!(check(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]), Ok(Kind::Jpeg));
        assert_eq!(check(b"RIFF\x10\0\0\0WEBPVP8 "), Ok(Kind::Webp));
        assert_eq!(
            check(b"PK\x03\x04 a docx"),
            Err(DocumentError::UnsupportedType)
        );
        assert_eq!(check(b"<html>%PDF-"), Err(DocumentError::UnsupportedType));
        assert_eq!(check(b""), Err(DocumentError::Empty));
        assert_eq!(
            check(&vec![b'%'; MAX_BYTES + 1]),
            Err(DocumentError::TooLarge(MAX_BYTES + 1))
        );
    }

    #[test]
    fn filenames_lose_paths_and_control_characters() {
        assert_eq!(clean_filename("../../etc/passwd"), "passwd");
        assert_eq!(
            clean_filename("C:\\Users\\a\\Invoice 42.pdf"),
            "Invoice 42.pdf"
        );
        assert_eq!(clean_filename("bad\r\nname.pdf"), "badname.pdf");
        assert_eq!(clean_filename("   "), "document");
    }
}
