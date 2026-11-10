//! Reading an inbound email: who sent it, and which of its parts are
//! documents worth putting in the inbox. Pure: the server stores and files
//! what this returns.
//!
//! Which parts count:
//! - PDFs and TIFFs, wherever they sit in the message.
//! - Images sent as attachments. Inline images under 20 KB are skipped:
//!   they are almost always logos in a signature.
//! - The attachments of a forwarded email (`message/rfc822`), one level deep,
//!   since bills are often forwarded on from someone else's inbox.
//! - Anything else (Word files, spreadsheets, calendar invites) is listed as
//!   skipped with a reason, so a person can see what didn't come through.

use chrono::{DateTime, Utc};
use mail_parser::{MessageParser, MessagePart, MessageRef, PartKind};
use serde::Serialize;

use crate::documents::{self, DocumentError, Kind};

/// Cloudflare Email Routing accepts messages up to 25 MiB.
pub const MAX_RAW_BYTES: usize = 25 * 1024 * 1024;

const LOGO_BYTES: usize = 20 * 1024;

#[derive(Debug)]
pub struct Parsed {
    pub message_id: Option<String>,
    pub from_address: Option<String>,
    pub from_name: Option<String>,
    pub subject: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    /// The receiving server's SPF/DKIM/DMARC verdicts, kept for later trust
    /// decisions. Not acted on yet.
    pub authentication_results: Option<String>,
    pub attachments: Vec<Attachment>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug)]
pub struct Attachment {
    pub filename: String,
    pub kind: Kind,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub filename: String,
    pub reason: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EmailError {
    #[error("the email is empty")]
    Empty,
    #[error("the email is {0} bytes; the limit is 25 MB")]
    TooLarge(usize),
    #[error("not a readable email message")]
    Unreadable,
}

pub fn parse(raw: &[u8]) -> Result<Parsed, EmailError> {
    if raw.is_empty() {
        return Err(EmailError::Empty);
    }
    if raw.len() > MAX_RAW_BYTES {
        return Err(EmailError::TooLarge(raw.len()));
    }
    let message = MessageParser::default()
        .parse(raw)
        .ok_or(EmailError::Unreadable)?;
    let root = message.root();
    let from = root.from().and_then(|l| l.first());
    let mut out = Parsed {
        message_id: root
            .message_id()
            .map(|id| id.trim_matches(['<', '>']).to_owned())
            .filter(|s| !s.is_empty()),
        from_address: from.and_then(|m| m.address()).map(str::to_lowercase),
        from_name: from.and_then(|m| m.name()).map(str::to_owned),
        subject: root.subject().map(str::to_owned),
        sent_at: root
            .date()
            .and_then(|d| DateTime::from_timestamp(d.to_timestamp(), 0)),
        authentication_results: root
            .headers()
            .get("Authentication-Results")
            .and_then(|h| h.value().as_text().map(str::to_owned)),
        attachments: Vec::new(),
        skipped: Vec::new(),
    };
    collect(root, &mut out, 0);
    Ok(out)
}

fn collect(message: MessageRef<'_>, out: &mut Parsed, depth: u8) {
    // Every leaf, not just `attachments()`: mail-parser counts inline images
    // as part of the body, and a scanned invoice is sometimes sent inline.
    for part in message.parts() {
        match part.kind() {
            PartKind::Message(nested) if depth == 0 => collect(nested, out, depth + 1),
            PartKind::Message(_) => out.skipped.push(Skipped {
                filename: name_of(&part, out.attachments.len(), None),
                reason: "an email forwarded inside a forwarded email".into(),
            }),
            PartKind::Binary | PartKind::InlineBinary => take(&part, out),
            PartKind::Text | PartKind::Html if part.is_attachment() => take(&part, out),
            _ => {}
        }
    }
}

fn take(part: &MessagePart<'_>, out: &mut Parsed) {
    let bytes = part.decoded();
    match documents::check(&bytes) {
        Ok(kind) => {
            let filename = name_of(part, out.attachments.len(), Some(kind));
            let is_image = !matches!(kind, Kind::Pdf | Kind::Tiff);
            if is_image && part.is_inline() && bytes.len() < LOGO_BYTES {
                out.skipped.push(Skipped {
                    filename,
                    reason: "a small inline image, probably a logo".into(),
                });
                return;
            }
            if out.attachments.iter().any(|a| a.bytes == *bytes) {
                return; // the same file attached twice
            }
            out.attachments.push(Attachment {
                filename,
                kind,
                bytes: bytes.into_owned(),
            });
        }
        Err(e) => {
            let reason = match e {
                DocumentError::Empty => return,
                DocumentError::TooLarge(_) => e.to_string(),
                DocumentError::UnsupportedType => {
                    let ctype = part.content_type().map(|c| match c.subtype() {
                        Some(sub) => format!("{}/{sub}", c.ctype()),
                        None => c.ctype().to_owned(),
                    });
                    format!(
                        "not a PDF or image ({})",
                        ctype.as_deref().unwrap_or("unknown type")
                    )
                }
            };
            out.skipped.push(Skipped {
                filename: name_of(part, out.attachments.len(), None),
                reason,
            });
        }
    }
}

fn name_of(part: &MessagePart<'_>, index: usize, kind: Option<Kind>) -> String {
    match part.attachment_name() {
        Some(n) if !n.trim().is_empty() => documents::clean_filename(n),
        _ => match kind {
            Some(k) => format!("attachment-{}.{}", index + 1, k.extension()),
            None => format!("attachment-{}", index + 1),
        },
    }
}

/// The routing token in an inbound address: `k7f3q9x2m4p8@in.payvy.com`
/// gives `k7f3q9x2m4p8` when the domain matches. A `+tag` after the token is
/// allowed and ignored, so people can label what they forward.
pub fn address_token(recipient: &str, domain: &str) -> Option<String> {
    let (local, host) = recipient.trim().trim_matches(['<', '>']).rsplit_once('@')?;
    if !host.eq_ignore_ascii_case(domain) {
        return None;
    }
    let token = local.split('+').next()?.to_ascii_lowercase();
    valid_token(&token).then_some(token)
}

pub fn valid_token(token: &str) -> bool {
    token.len() == TOKEN_LEN && token.bytes().all(|b| TOKEN_ALPHABET.contains(&b))
}

/// Lowercase letters and digits without look-alikes (0/o, 1/l/i), so an
/// address read aloud or retyped still works.
pub const TOKEN_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
pub const TOKEN_LEN: usize = 12;

#[cfg(test)]
mod tests {
    use super::*;

    const PDF_B64: &str = "JVBERi0xLjQKJSBpbnZvaWNlIDE4OTcyNwolJUVPRgo="; // "%PDF-1.4\n% invoice 189727\n%%EOF\n"

    fn email(parts: &str) -> Vec<u8> {
        format!(
            "From: King Environmental <AR@KingEnv.com>\r\n\
             To: k7f3q9x2m4p8@in.payvy.com\r\n\
             Subject: Invoice 189727\r\n\
             Date: Mon, 9 Nov 2026 10:00:00 -0800\r\n\
             Message-ID: <abc123@kingenv.com>\r\n\
             Authentication-Results: mx.cloudflare.net; spf=pass; dkim=pass\r\n\
             Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
             --b\r\nContent-Type: text/plain\r\n\r\nPlease find attached.\r\n\
             {parts}--b--\r\n"
        )
        .into_bytes()
    }

    fn attachment(ctype: &str, disposition: &str, name: &str, b64: &str) -> String {
        format!(
            "--b\r\nContent-Type: {ctype}\r\nContent-Disposition: {disposition}; filename=\"{name}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{b64}\r\n"
        )
    }

    #[test]
    fn reads_the_sender_and_takes_the_pdf() {
        let raw = email(&attachment(
            "application/pdf",
            "attachment",
            "INV-189727.pdf",
            PDF_B64,
        ));
        let p = parse(&raw).unwrap();
        assert_eq!(p.message_id.as_deref(), Some("abc123@kingenv.com"));
        assert_eq!(p.from_address.as_deref(), Some("ar@kingenv.com"));
        assert_eq!(p.from_name.as_deref(), Some("King Environmental"));
        assert_eq!(p.subject.as_deref(), Some("Invoice 189727"));
        assert_eq!(p.sent_at.unwrap().to_rfc3339(), "2026-11-09T18:00:00+00:00");
        assert!(p.authentication_results.unwrap().contains("dkim=pass"));
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].filename, "INV-189727.pdf");
        assert_eq!(p.attachments[0].kind, Kind::Pdf);
        assert!(p.attachments[0].bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn type_comes_from_the_bytes_and_skips_are_explained() {
        // Labelled octet-stream but really a PDF; a Word file; a tiny inline logo.
        let logo = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/";
        let raw = email(&format!(
            "{}{}{}",
            attachment("application/octet-stream", "attachment", "scan", PDF_B64),
            attachment(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "attachment",
                "terms.docx",
                "UEsDBBQAAAAI"
            ),
            attachment("image/jpeg", "inline", "logo.jpg", logo),
        ));
        let p = parse(&raw).unwrap();
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].filename, "scan");
        assert_eq!(p.skipped.len(), 2, "{:?}", p.skipped);
        assert_eq!(p.skipped[0].filename, "terms.docx");
        assert!(
            p.skipped[0]
                .reason
                .starts_with("not a PDF or image (application/vnd.openxml"),
            "{:?}",
            p.skipped[0]
        );
        assert_eq!(p.skipped[1].reason, "a small inline image, probably a logo");
    }

    #[test]
    fn takes_attachments_from_a_forwarded_email() {
        let inner = format!(
            "From: vendor@example.com\r\nSubject: Inv 7\r\nContent-Type: multipart/mixed; boundary=\"i\"\r\n\r\n\
             --i\r\nContent-Type: text/plain\r\n\r\nattached\r\n\
             --i\r\nContent-Type: application/pdf; name=\"inv7.pdf\"\r\nContent-Disposition: attachment; filename=\"inv7.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{PDF_B64}\r\n--i--\r\n"
        );
        let raw = email(&format!(
            "--b\r\nContent-Type: message/rfc822\r\nContent-Disposition: attachment; filename=\"fwd.eml\"\r\n\r\n{inner}\r\n"
        ));
        let p = parse(&raw).unwrap();
        assert_eq!(
            p.from_address.as_deref(),
            Some("ar@kingenv.com"),
            "the sender is who forwarded it"
        );
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].filename, "inv7.pdf");
    }

    #[test]
    fn rejects_garbage_and_oversize() {
        assert_eq!(parse(b"").unwrap_err(), EmailError::Empty);
        assert_eq!(
            parse(&vec![b'a'; MAX_RAW_BYTES + 1]).unwrap_err(),
            EmailError::TooLarge(MAX_RAW_BYTES + 1)
        );
    }

    #[test]
    fn address_tokens() {
        let d = "in.payvy.com";
        assert_eq!(
            address_token("k7f3q9x2m4p8@in.payvy.com", d).as_deref(),
            Some("k7f3q9x2m4p8")
        );
        assert_eq!(
            address_token("<K7F3Q9X2M4P8+fuel@IN.payvy.com>", d).as_deref(),
            Some("k7f3q9x2m4p8")
        );
        assert_eq!(address_token("k7f3q9x2m4p8@evil.com", d), None);
        assert_eq!(address_token("short@in.payvy.com", d), None);
        assert_eq!(
            address_token("k7f3q9x2m4p0@in.payvy.com", d),
            None,
            "0 isn't in the alphabet"
        );
    }
}
