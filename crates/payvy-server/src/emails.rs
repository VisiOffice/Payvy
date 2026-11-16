//! The words of the mail Payvy sends about signing in: the ordinary sign-in
//! link, and the welcome a newly created company's first Admin gets instead.
//!
//! Every message has a text part that is complete on its own, with the full
//! link, and an HTML part in one plain frame: inline styles only, one column,
//! no images, no fonts or anything else fetched from elsewhere, no tracking,
//! and the link exactly as issued. The frame sets no page background or body
//! text colour and declares `color-scheme: light dark`, so a mail client's
//! own light or dark colours apply; the few colours it does set (the button,
//! the rule, the closing line) read on both.
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

const P: &str = "margin:0 0 16px;";

/// The frame every HTML mail shares. `title` and `body` are already HTML.
fn frame(title: &str, body: &str) -> String {
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
            "<div style=\"max-width:560px;margin:0 auto;padding:24px 20px;",
            "font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;",
            "font-size:16px;line-height:1.5;\">\n",
            "<p style=\"margin:0 0 24px;font-size:22px;font-weight:700;letter-spacing:-0.02em;\">Payvy</p>\n",
            "{body}",
            "</div>\n",
            "</body>\n",
            "</html>\n",
        ),
        title = title,
        body = body,
    )
}

/// The link as a button, and again as text under it so it can be copied.
fn button_and_link(link: &str) -> String {
    let href = escape(link);
    format!(
        concat!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"margin:0 0 16px;\">",
            "<tr><td style=\"background-color:#1d4ed8;border-radius:6px;\">",
            "<a href=\"{href}\" style=\"display:inline-block;padding:12px 22px;color:#ffffff;",
            "font-weight:600;text-decoration:none;\">Sign in to Payvy</a>",
            "</td></tr></table>\n",
            "<p style=\"{P}\">Or copy this link into your browser:<br>",
            "<a href=\"{href}\" style=\"word-break:break-all;\">{href}</a></p>\n",
        ),
        href = href,
        P = P,
    )
}

/// The ordinary sign-in mail (`/auth/link`).
pub fn sign_in(to: &str, link: &str, link_minutes: i64) -> Mail {
    let rules = format!(
        "The link works once, for {link_minutes} minutes. If you didn't ask for it, ignore this email: nobody can sign in without opening it."
    );
    let html = frame(
        "Sign in to Payvy",
        &format!(
            "<p style=\"{P}\">Open this link to sign in to Payvy:</p>\n{}<p style=\"{P}\">{rules}</p>\n",
            button_and_link(link),
        ),
    );
    Mail {
        to: to.to_owned(),
        subject: "Sign in to Payvy".into(),
        text: format!("Open this link to sign in to Payvy:\n\n{link}\n\n{rules}\n"),
        html: Some(html),
    }
}

/// The first mail a newly created company's first Admin gets, in place of
/// the sign-in mail. Each step is something the product does for a company
/// that has just been created.
pub fn welcome(w: &Welcome<'_>) -> Mail {
    let sign_in_page = format!("{}/sign-in", w.public_url);
    let created = format!("{} was created in Payvy for {}.", w.company, w.to);
    let rules = format!(
        "The link works once, for {} minutes. If it has expired, ask for a new one at {sign_in_page}. If you didn't ask for it, ignore this email: nobody can sign in without opening it.",
        w.link_minutes
    );
    let first_bill = if w.email_intake {
        "Send a first bill. Upload an invoice in the Inbox, or create your company's email-in address under Settings > Email-in address and forward bills to it."
    } else {
        "Send a first bill. Upload an invoice in the Inbox."
    };
    let steps = [
        first_bill.to_owned(),
        "Connect the bank that pays, under Settings > Bank. Payvy pays through Mercury today, from your own account: it never holds your money.".to_owned(),
        format!(
            "Add the people who approve, under Settings > People. Payvy doesn't email them: each asks for a sign-in link at {sign_in_page}."
        ),
    ];

    let mut text = format!(
        "{created}\n\nOpen this link to sign in:\n\n{}\n\n{rules}\n\nWhat to do first:\n\n",
        w.link
    );
    for (i, step) in steps.iter().enumerate() {
        text.push_str(&format!("{}. {step}\n", i + 1));
    }
    if let Some(note) = w.note {
        text.push_str(&format!("\n{note}\n"));
    }
    text.push_str(&format!("\nPayvy: {}\n", w.public_url));

    let mut body = format!(
        "<p style=\"{P}\">{}</p>\n{}<p style=\"{P}\">{}</p>\n<p style=\"margin:24px 0 8px;font-weight:700;\">What to do first</p>\n<ol style=\"margin:0 0 16px;padding:0 0 0 24px;\">\n",
        escape(&created),
        button_and_link(w.link),
        escape(&rules),
    );
    for step in &steps {
        body.push_str(&format!(
            "<li style=\"margin:0 0 8px;\">{}</li>\n",
            escape(step)
        ));
    }
    body.push_str("</ol>\n");
    if let Some(note) = w.note {
        // Escaped text with its line breaks kept; never an anchor.
        body.push_str(&format!(
            "<p style=\"{P}\">{}</p>\n",
            escape(note).replace('\n', "<br>")
        ));
    }
    let url = escape(w.public_url);
    body.push_str(&format!(
        "<p style=\"margin:24px 0 0;padding:16px 0 0;border-top:1px solid #8a8a8a;font-size:14px;color:#767676;\">Payvy: <a href=\"{url}\" style=\"color:#767676;\">{url}</a></p>\n"
    ));

    let subject = format!("Welcome to Payvy: {} is ready", w.company);
    Mail {
        to: w.to.to_owned(),
        html: Some(frame(&escape(&subject), &body)),
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
        assert!(!html.contains("What to do first"));
    }

    #[test]
    fn the_welcome_names_the_company_and_carries_the_link_in_both_parts() {
        let mail = welcome(&sample("Quarry Rentals", None));
        assert_eq!(mail.subject, "Welcome to Payvy: Quarry Rentals is ready");
        assert!(
            mail.text
                .starts_with("Quarry Rentals was created in Payvy for robert@example.com.\n")
        );
        assert!(mail.text.contains(LINK));
        assert!(mail.text.contains(
            "If it has expired, ask for a new one at https://app.payvy.example/sign-in."
        ));
        assert!(mail.text.contains(
            "If you didn't ask for it, ignore this email: nobody can sign in without opening it."
        ));
        assert!(mail.text.ends_with("\nPayvy: https://app.payvy.example\n"));
        let html = mail.html.unwrap();
        assert_eq!(html.matches(&format!("href=\"{LINK}\"")).count(), 2);
        assert!(html.contains("Quarry Rentals was created in Payvy for robert@example.com."));
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
            "&lt;script&gt;alert(1)&lt;/script&gt; &amp; Sons &quot;Ltd&quot; was created"
        ));
        assert!(html.contains("<title>Welcome to Payvy: &lt;script&gt;"));
        assert!(html.contains(
            "&lt;a href=&quot;https://evil.example&quot;&gt;Free&lt;/a&gt; &amp; &lt;b&gt;bold&lt;/b&gt;"
        ));
        // The note made no link: the only anchors are the frame's own.
        assert_eq!(html.matches("<a ").count(), 3);
    }

    #[test]
    fn the_note_is_its_own_paragraph_or_absent() {
        let with = welcome(&sample("Quarry Rentals", Some("Line one.\nLine two.")));
        assert!(
            with.text
                .contains("\n\nLine one.\nLine two.\n\nPayvy: https://app.payvy.example\n")
        );
        assert!(
            with.html
                .as_deref()
                .unwrap()
                .contains("<p style=\"margin:0 0 16px;\">Line one.<br>Line two.</p>\n")
        );

        let without = welcome(&sample("Quarry Rentals", None));
        assert!(
            without
                .text
                .contains("sign-in.\n\nPayvy: https://app.payvy.example\n")
        );
        let html = without.html.unwrap();
        assert!(html.contains("</ol>\n<p style=\"margin:24px 0 0;"));
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
                .contains("1. Send a first bill. Upload an invoice in the Inbox.\n")
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
