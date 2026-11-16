//! The words of the mail Payvy sends about signing in: the ordinary sign-in
//! link, and the welcome a newly created company's first Admin gets instead.
//!
//! Every message has a text part that is complete on its own, with the full
//! link, and an HTML part in one plain frame: inline styles only, one column,
//! no images, no fonts or anything else fetched from elsewhere, no tracking,
//! and the link exactly as issued. The frame sets no page background or body
//! text colour and declares `color-scheme: light dark`, so a mail client's
//! own light or dark colours apply; the few colours it does set (the brand
//! red of the wordmark's dot, the button, the note's bar and the step numbers,
//! and greys for fine print and the rule) read on both. A hidden first line
//! is what an inbox shows beside the subject.
//!
//! Everything a person supplied (company name, address, the operator's note)
//! goes through `escape` before it reaches the HTML part. This is open-source
//! and self-hosted by others, so nothing here names a price, a plan, a trial
//! or a site: the only address is the configured `PAYVY_PUBLIC_URL`, and
//! anything about terms arrives in the operator's `welcome_note`.

use crate::mailer::Mail;

/// What the welcome mail says, beyond the link.
pub struct Welcome<'a> {
    pub to: &'a str,
    /// The company's name as its people see it.
    pub company: &'a str,
    /// The sign-in link, opening in the new company.
    pub link: &'a str,
    /// `PAYVY_PUBLIC_URL`, without a trailing slash.
    pub public_url: &'a str,
    /// How long the link lasts.
    pub link_minutes: i64,
    /// Whether this server takes bills by email (`PAYVY_INBOUND_DOMAIN`): a
    /// new company has no address until a person creates one, so the step
    /// says where, and is left out where email intake is off.
    pub email_intake: bool,
    /// The operator's `welcome_note`, already validated: its own paragraph.
    pub note: Option<&'a str>,
}

/// `& < > " '` as entities, for text and for quoted attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

// Colours that read on a light or a dark client background: the brand red
// with white on it, a mid grey for fine print and a light grey rule.
const RED: &str = "#dc3e46";
const RED_EDGE: &str = "#b62b32";
const MUTED: &str = "#767676";
const RULE: &str = "#d4d4d0";
const FONT: &str =
    "-apple-system,BlinkMacSystemFont,'Segoe UI',Inter,Roboto,Helvetica,Arial,sans-serif";

/// The frame every HTML mail shares: the wordmark, the body and a closing
/// line. `title`, `preview` and `body` are already HTML; `preview` is the
/// line an inbox shows beside the subject, hidden in the message itself.
fn frame(title: &str, preview: &str, body: &str, public_url: &str) -> String {
    let url = escape(public_url);
    let host = escape(
        public_url
            .trim_start_matches("https://")
            .trim_start_matches("http://"),
    );
    format!(
        concat!(
            "<!doctype html>\n",
            "<html lang=\"en\">\n",
            "<head>\n",
            "<meta charset=\"utf-8\">\n",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
            "<meta name=\"color-scheme\" content=\"light dark\">\n",
            "<meta name=\"supported-color-schemes\" content=\"light dark\">\n",
            "<title>{title}</title>\n",
            "</head>\n",
            "<body style=\"margin:0;padding:0;color-scheme:light dark;\">\n",
            "<div style=\"display:none;max-height:0;overflow:hidden;opacity:0;\">{preview}</div>\n",
            "<div style=\"max-width:520px;margin:0 auto;padding:32px 24px;",
            "font-family:{FONT};font-size:16px;line-height:1.55;\">\n",
            "<p style=\"margin:0 0 36px;font-size:26px;font-weight:760;letter-spacing:-1px;line-height:1;\">",
            "Payvy<span style=\"color:{RED};\">.</span></p>\n",
            "{body}",
            "<p style=\"margin:32px 0 0;font-size:13px;color:{MUTED};\">",
            "Payvy &middot; <a href=\"{url}\" style=\"color:{MUTED};\">{host}</a></p>\n",
            "</div>\n",
            "</body>\n",
            "</html>\n",
        ),
        title = title,
        preview = preview,
        body = body,
        url = url,
        host = host,
        FONT = FONT,
        RED = RED,
        MUTED = MUTED,
    )
}

/// The red button, and the fine print under it.
fn button(link: &str, label: &str, under: &str) -> String {
    format!(
        concat!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:0 0 12px;\">",
            "<tr><td style=\"background-color:{RED};border-radius:6px;border-bottom:3px solid {RED_EDGE};\">",
            "<a href=\"{href}\" style=\"display:inline-block;padding:13px 24px;color:#ffffff;",
            "font-size:16px;font-weight:600;text-decoration:none;\">{label} &rarr;</a>",
            "</td></tr></table>\n",
            "<p style=\"margin:0 0 32px;font-size:14px;color:{MUTED};\">{under}</p>\n",
        ),
        href = escape(link),
        label = label,
        under = under,
        RED = RED,
        RED_EDGE = RED_EDGE,
        MUTED = MUTED,
    )
}

/// Fine print under a rule: why the mail came, and the link to copy if the
/// button doesn't work.
fn fine_print(why: &str, link: &str) -> String {
    let href = escape(link);
    format!(
        concat!(
            "<div style=\"margin:32px 0 0;padding:20px 0 0;border-top:1px solid {RULE};",
            "font-size:13px;line-height:1.6;color:{MUTED};\">\n",
            "<p style=\"margin:0 0 12px;\">{why}</p>\n",
            "<p style=\"margin:0;\">Button not working? Copy this link into your browser:<br>",
            "<a href=\"{href}\" style=\"color:{MUTED};word-break:break-all;\">{href}</a></p>\n",
            "</div>\n",
        ),
        why = why,
        href = href,
        RULE = RULE,
        MUTED = MUTED,
    )
}

/// The ordinary sign-in mail (`/auth/link`).
pub fn sign_in(to: &str, link: &str, link_minutes: i64) -> Mail {
    let rules = format!(
        "The link works once, for {link_minutes} minutes. If you didn't ask for it, ignore this email: nobody can sign in without opening it."
    );
    // Links are always `{PAYVY_PUBLIC_URL}/sign-in…` (auth::email_link).
    let public_url = link.split("/sign-in").next().unwrap_or(link);
    let body = format!(
        concat!(
            "<h1 style=\"margin:0 0 12px;font-size:24px;font-weight:650;letter-spacing:-0.5px;line-height:1.25;\">Sign in to Payvy</h1>\n",
            "<p style=\"margin:0 0 24px;\">Here's the link you asked for.</p>\n",
            "{button}",
            "{fine}",
        ),
        button = button(
            link,
            "Sign in to Payvy",
            &format!("Works once, for {link_minutes} minutes.")
        ),
        fine = fine_print(
            "If you didn't ask for it, ignore this email: nobody can sign in without opening it.",
            link
        ),
    );
    Mail {
        to: to.to_owned(),
        subject: "Sign in to Payvy".into(),
        text: format!("Open this link to sign in to Payvy:\n\n{link}\n\n{rules}\n"),
        html: Some(frame(
            "Sign in to Payvy",
            "Your sign-in link for Payvy.",
            &body,
            public_url,
        )),
    }
}

/// One step of the welcome: a short title and what to do.
struct Step {
    title: &'static str,
    detail: String,
}

/// The first mail a newly created company's first Admin gets, in place of
/// the sign-in mail. Each step is something the product does for a company
/// that has just been created.
pub fn welcome(w: &Welcome<'_>) -> Mail {
    let sign_in_page = format!("{}/sign-in", w.public_url);
    let steps = [
        Step {
            title: "Send your first bill",
            detail: if w.email_intake {
                "Upload an invoice in the Inbox, or create your email-in address under Settings > Email-in address and forward bills to it.".into()
            } else {
                "Upload an invoice in the Inbox.".into()
            },
        },
        Step {
            title: "Connect the bank that pays",
            detail: "Under Settings > Bank. Payvy pays through Mercury from your own account, and never holds your money.".into(),
        },
        Step {
            title: "Add the people who approve",
            detail: format!(
                "Under Settings > People. Each of them signs in with their email at {sign_in_page}."
            ),
        },
    ];
    let why = format!(
        "{} was created in Payvy for {}. If you didn't ask for it, ignore this email: nobody can sign in without opening the link.",
        w.company, w.to
    );
    let expired = format!(
        "The link works once, for {} minutes. If it has expired, ask for a new one at {sign_in_page}.",
        w.link_minutes
    );

    // The text part: complete on its own, in the same order.
    let mut text = format!(
        "{} is ready in Payvy.\n\nSign in:\n\n{}\n\n{expired}\n\n",
        w.company, w.link
    );
    if let Some(note) = w.note {
        text.push_str(&format!("{note}\n\n"));
    }
    text.push_str("Three steps to your first payment:\n\n");
    for (i, step) in steps.iter().enumerate() {
        text.push_str(&format!("{}. {}. {}\n", i + 1, step.title, step.detail));
    }
    text.push_str(&format!("\n{why}\n\nPayvy: {}\n", w.public_url));

    let company = escape(w.company);
    let mut body = format!(
        "<h1 style=\"margin:0 0 12px;font-size:26px;font-weight:650;letter-spacing:-0.6px;line-height:1.2;\">{company} is ready</h1>\n\
         <p style=\"margin:0 0 24px;\">Welcome to Payvy. Sign in to send your first bill.</p>\n"
    );
    if let Some(note) = w.note {
        // Escaped text with its line breaks kept; never an anchor.
        body.push_str(&format!(
            "<p style=\"margin:0 0 28px;padding:2px 0 2px 14px;border-left:3px solid {RED};\">{}</p>\n",
            escape(note).replace('\n', "<br>")
        ));
    }
    body.push_str(&button(
        w.link,
        "Sign in to Payvy",
        &format!(
            "Works once, for {} minutes. Expired? <a href=\"{}\" style=\"color:{MUTED};\">Ask for a new link</a>.",
            w.link_minutes,
            escape(&sign_in_page)
        ),
    ));
    body.push_str(&format!(
        "<p style=\"margin:0 0 16px;font-size:12px;font-weight:600;letter-spacing:1.2px;text-transform:uppercase;color:{MUTED};\">Three steps to your first payment</p>\n\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"width:100%;\">\n"
    ));
    for (i, step) in steps.iter().enumerate() {
        body.push_str(&format!(
            concat!(
                "<tr>",
                "<td style=\"width:28px;padding:0 14px 20px 0;vertical-align:top;\">",
                "<div style=\"width:26px;height:26px;border:1.5px solid {RED};border-radius:50%;",
                "text-align:center;line-height:26px;font-size:13px;font-weight:700;color:{RED};\">{n}</div></td>",
                "<td style=\"padding:0 0 20px;vertical-align:top;\">",
                "<p style=\"margin:0 0 2px;font-weight:600;\">{title}</p>",
                "<p style=\"margin:0;font-size:15px;color:{MUTED};\">{detail}</p>",
                "</td></tr>\n",
            ),
            n = i + 1,
            title = step.title,
            detail = escape(&step.detail),
            RED = RED,
            MUTED = MUTED,
        ));
    }
    body.push_str("</table>\n");
    body.push_str(&fine_print(&escape(&why), w.link));

    let subject = format!("Welcome to Payvy: {} is ready", w.company);
    let preview = format!("Sign in to {company} and send your first bill.");
    Mail {
        to: w.to.to_owned(),
        html: Some(frame(&escape(&subject), &preview, &body, w.public_url)),
        subject,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "https://app.payvy.example/sign-in#token=0000000000000000000000000000000000000000000000000000000000000000";

    fn sample<'a>(company: &'a str, note: Option<&'a str>) -> Welcome<'a> {
        Welcome {
            to: "robert@example.com",
            company,
            link: LINK,
            public_url: "https://app.payvy.example",
            link_minutes: 15,
            email_intake: true,
            note,
        }
    }

    #[test]
    fn the_sign_in_text_is_what_it_always_was() {
        let mail = sign_in("robert@example.com", LINK, 15);
        assert_eq!(mail.subject, "Sign in to Payvy");
        assert_eq!(
            mail.text,
            format!(
                "Open this link to sign in to Payvy:\n\n{LINK}\n\nThe link works once, for 15 minutes. If you didn't ask for it, ignore this email: nobody can sign in without opening it.\n"
            )
        );
        let html = mail.html.unwrap();
        // The link is the button's target and is shown again to copy.
        assert_eq!(html.matches(&format!("href=\"{LINK}\"")).count(), 2);
        assert!(html.contains(&format!(">{LINK}</a>")));
        assert!(html.contains("nobody can sign in without opening it."));
        assert!(!html.contains("Three steps"));
    }

    #[test]
    fn the_welcome_names_the_company_and_carries_the_link_in_both_parts() {
        let mail = welcome(&sample("Quarry Rentals", None));
        assert_eq!(mail.subject, "Welcome to Payvy: Quarry Rentals is ready");
        assert!(
            mail.text
                .starts_with("Quarry Rentals is ready in Payvy.\n\nSign in:\n\n")
        );
        assert!(mail.text.contains(LINK));
        assert!(mail.text.contains(
            "If it has expired, ask for a new one at https://app.payvy.example/sign-in."
        ));
        assert!(mail.text.contains(
            "Quarry Rentals was created in Payvy for robert@example.com. If you didn't ask for it, ignore this email: nobody can sign in without opening the link."
        ));
        assert!(mail.text.ends_with("\nPayvy: https://app.payvy.example\n"));
        let html = mail.html.unwrap();
        assert_eq!(html.matches(&format!("href=\"{LINK}\"")).count(), 2);
        assert!(html.contains(">Quarry Rentals is ready</h1>"));
        assert!(html.contains("Quarry Rentals was created in Payvy for robert@example.com."));
        // The inbox preview line, hidden in the message itself.
        assert!(html.contains(
            "display:none;max-height:0;overflow:hidden;opacity:0;\">Sign in to Quarry Rentals and send your first bill.</div>"
        ));
    }

    #[test]
    fn html_mail_fetches_nothing_and_styles_inline() {
        for mail in [
            sign_in("robert@example.com", LINK, 15),
            welcome(&sample("Quarry Rentals", Some("A note."))),
        ] {
            let html = mail.html.unwrap();
            for banned in [
                "<img",
                "<style",
                "<link",
                "<script",
                "@import",
                "@font-face",
                "url(",
                "src=",
                "background=",
            ] {
                assert!(!html.contains(banned), "{banned} in {html}");
            }
            assert!(html.contains("<meta name=\"color-scheme\" content=\"light dark\">"));
            // Every link goes to the configured app and nowhere else.
            for (at, _) in html.match_indices("href=\"") {
                assert!(
                    html[at + 6..].starts_with("https://app.payvy.example"),
                    "{}",
                    &html[at..at + 60]
                );
            }
        }
    }

    #[test]
    fn what_people_typed_is_escaped_in_the_html_part_only() {
        let company = "<script>alert(1)</script> & Sons \"Ltd\"";
        let note = "<a href=\"https://evil.example\">Free</a> & <b>bold</b>";
        let mail = welcome(&sample(company, Some(note)));
        // The text part and the subject say exactly what was typed.
        assert!(mail.subject.contains(company));
        assert!(mail.text.contains(company));
        assert!(mail.text.contains(note));
        let html = mail.html.unwrap();
        assert!(!html.contains("<script"));
        assert!(!html.contains("<b>"));
        assert!(!html.contains("evil.example\">"));
        assert!(html.contains(
            "&lt;script&gt;alert(1)&lt;/script&gt; &amp; Sons &quot;Ltd&quot; is ready</h1>"
        ));
        assert!(html.contains(
            "&lt;script&gt;alert(1)&lt;/script&gt; &amp; Sons &quot;Ltd&quot; was created"
        ));
        assert!(html.contains("<title>Welcome to Payvy: &lt;script&gt;"));
        assert!(html.contains(
            "&lt;a href=&quot;https://evil.example&quot;&gt;Free&lt;/a&gt; &amp; &lt;b&gt;bold&lt;/b&gt;"
        ));
        // The note made no link: the only anchors are the mail's own (the
        // button, a new link, the link to copy and the closing line).
        assert_eq!(html.matches("<a ").count(), 4);
    }

    #[test]
    fn the_note_is_its_own_paragraph_or_absent() {
        let with = welcome(&sample("Quarry Rentals", Some("Line one.\nLine two.")));
        assert!(
            with.text
                .contains("\n\nLine one.\nLine two.\n\nThree steps to your first payment:\n")
        );
        assert!(
            with.html
                .as_deref()
                .unwrap()
                .contains("border-left:3px solid #dc3e46;\">Line one.<br>Line two.</p>\n")
        );

        let without = welcome(&sample("Quarry Rentals", None));
        assert!(
            without
                .text
                .contains("/sign-in.\n\nThree steps to your first payment:\n")
        );
        let html = without.html.unwrap();
        assert!(!html.contains("border-left:3px"));
        assert_eq!(
            html.matches("<p ").count() + 1,
            with.html.unwrap().matches("<p ").count()
        );
    }

    #[test]
    fn the_email_in_step_is_only_offered_where_email_intake_is_on() {
        let mut w = sample("Quarry Rentals", None);
        assert!(welcome(&w).text.contains("Settings > Email-in address"));
        w.email_intake = false;
        let mail = welcome(&w);
        assert!(!mail.text.contains("Email-in"));
        assert!(
            mail.text
                .contains("1. Send your first bill. Upload an invoice in the Inbox.\n")
        );
        assert!(!mail.html.unwrap().contains("Email-in"));
    }

    #[test]
    fn the_core_states_no_price_plan_or_site_of_its_own() {
        let mail = welcome(&sample("Quarry Rentals", None));
        for part in [mail.text.to_lowercase(), mail.html.unwrap().to_lowercase()] {
            for word in [
                "trial",
                "plan",
                "price",
                "$",
                "payvy.com",
                "billing",
                "unsubscribe",
            ] {
                assert!(!part.contains(word), "{word}");
            }
        }
    }

    /// Writes both mails where `PAYVY_MAIL_PREVIEW_DIR` points, to open in a
    /// browser: `PAYVY_MAIL_PREVIEW_DIR=/tmp/mail cargo test -p payvy-server --lib emails`.
    #[test]
    fn preview_files() {
        let Ok(dir) = std::env::var("PAYVY_MAIL_PREVIEW_DIR") else {
            return;
        };
        let dir = std::path::Path::new(&dir);
        let note = "Your 14-day trial has started, no card needed. Add a plan in Settings > Billing before it ends.";
        for (name, mail) in [
            ("welcome", welcome(&sample("Quarry Rentals", Some(note)))),
            ("sign-in", sign_in("robert@example.com", LINK, 15)),
        ] {
            std::fs::write(dir.join(format!("{name}.html")), mail.html.unwrap()).unwrap();
            std::fs::write(dir.join(format!("{name}.txt")), mail.text).unwrap();
        }
    }
}
