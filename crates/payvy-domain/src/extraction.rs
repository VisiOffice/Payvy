//! Reading a bill from its document. A model (Claude, or anything with an
//! OpenAI-compatible API) fills in [`Read`], which is the document as
//! printed: amounts are the strings on the page, not numbers the model
//! converted. Everything after that is here and deterministic: turning the
//! strings into cents, checking that the numbers agree, matching a vendor.
//! A weak model then produces a draft that fails its checks, never a wrong
//! bill.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Cents;
use crate::bills::{Line, invoice_key};

/// What the model is told. The schema carries the field-by-field detail.
pub const INSTRUCTIONS: &str = "\
You are reading one document that a business received, for its accounts payable. \
Fill in the JSON schema from what is printed on the document. \
Copy amounts exactly as printed, including the decimal point (\"1,234.56\"); don't add currency \
symbols, do arithmetic or round. Write dates as YYYY-MM-DD. Use null for anything the document \
doesn't show; never guess. \
List the line items as printed. Put tax in `tax`, not in the lines. Shipping, fees and \
discounts that are printed as their own lines are lines (a discount's amount is negative). \
`kind` is what the document is: an invoice asks to be paid; a statement lists invoices or a \
balance; a credit memo reduces what is owed; a receipt shows something already paid. \
If anything would make a careful bookkeeper hesitate (a past-due notice, a changed bank \
account, handwriting, two invoices in one file, a total that doesn't match the lines), say so \
in `notes`.";

/// The model's answer, as printed on the document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Read {
    pub kind: Kind,
    pub vendor_name: Option<String>,
    pub vendor_email: Option<String>,
    pub vendor_address: Option<String>,
    /// The customer account number the vendor uses for you.
    pub account_number: Option<String>,
    pub invoice_number: Option<String>,
    pub invoice_date: Option<String>,
    pub due_date: Option<String>,
    /// As printed, e.g. "Net 30".
    pub terms: Option<String>,
    /// ISO 4217, e.g. USD.
    pub currency: Option<String>,
    pub po_number: Option<String>,
    pub subtotal: Option<String>,
    pub tax: Option<String>,
    /// What the document says is owed.
    pub total: Option<String>,
    #[serde(default)]
    pub lines: Vec<ReadLine>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadLine {
    pub description: String,
    pub amount: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Invoice,
    Statement,
    CreditMemo,
    Receipt,
    Quote,
    #[default]
    Other,
}

/// The JSON Schema [`Read`] is filled in against. It follows the subset both
/// Claude's structured outputs and OpenAI's strict mode accept: every field
/// required (null when absent), no additional properties, no numeric or
/// length constraints.
pub fn schema() -> Value {
    fn text(description: &str) -> Value {
        json!({ "anyOf": [{ "type": "string" }, { "type": "null" }], "description": description })
    }
    let fields = [
        ("vendor_name", "The business the document is from"),
        ("vendor_email", "The vendor's email address, if printed"),
        ("vendor_address", "The vendor's address, one line"),
        (
            "account_number",
            "Your customer or account number with this vendor",
        ),
        ("invoice_number", "The invoice or document number"),
        ("invoice_date", "YYYY-MM-DD"),
        ("due_date", "YYYY-MM-DD, if printed"),
        ("terms", "Payment terms as printed, e.g. Net 30"),
        ("currency", "ISO 4217 code, e.g. USD"),
        ("po_number", "Purchase order number, if printed"),
        ("subtotal", "Amount before tax, exactly as printed"),
        ("tax", "Total tax, exactly as printed"),
        (
            "total",
            "The amount the document says is owed, exactly as printed",
        ),
        (
            "notes",
            "Anything a careful bookkeeper should know before paying, or null",
        ),
    ];
    let mut properties = serde_json::Map::new();
    properties.insert(
        "kind".into(),
        json!({ "type": "string", "enum": ["invoice", "statement", "credit_memo", "receipt", "quote", "other"] }),
    );
    for (name, description) in fields {
        properties.insert(name.into(), text(description));
    }
    properties.insert(
        "lines".into(),
        json!({
            "type": "array",
            "description": "Line items as printed, without tax",
            "items": {
                "type": "object",
                "properties": {
                    "description": { "type": "string" },
                    "amount": { "type": "string", "description": "The line's total, exactly as printed; negative for a discount" }
                },
                "required": ["description", "amount"],
                "additionalProperties": false
            }
        }),
    );
    let required: Vec<&String> = properties.keys().collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

/// "1,234.56", "$1,234.56", "-12.00", "(12.00)" or "12" to cents. Refuses
/// more than two decimals and anything that isn't plainly a number, rather
/// than guessing.
pub fn parse_money(s: &str) -> Result<Cents, String> {
    let original = s;
    let mut s = s.trim();
    let mut negative = false;
    if let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        negative = true;
        s = inner.trim();
    }
    if let Some(rest) = s.strip_prefix('-') {
        negative = !negative;
        s = rest.trim();
    }
    let s = s.trim_start_matches(['$', '€', '£']).trim();
    let s = s.strip_suffix("USD").unwrap_or(s).trim();
    let bad = || format!("\"{original}\" isn't an amount Payvy can read");
    if s.is_empty() {
        return Err(bad());
    }
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    if frac.len() > 2 || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    // Thousands separators only between groups of three.
    let groups: Vec<&str> = whole.split(',').collect();
    let well_grouped = groups.len() == 1
        || (!groups[0].is_empty()
            && groups[0].len() <= 3
            && groups[1..].iter().all(|g| g.len() == 3));
    let digits: String = groups.concat();
    if !well_grouped || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let whole: i64 = digits.parse().map_err(|_| bad())?;
    let frac: i64 = format!("{frac:0<2}").parse().map_err(|_| bad())?;
    let cents = whole
        .checked_mul(100)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(bad)?;
    Ok(if negative { -cents } else { cents })
}

/// Payvy's reading of [`Read`]: amounts in cents, dates parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub kind: Kind,
    pub vendor_name: Option<String>,
    pub vendor_email: Option<String>,
    pub account_number: Option<String>,
    pub invoice_number: Option<String>,
    pub invoice_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub currency: Option<String>,
    pub po_number: Option<String>,
    pub subtotal_cents: Option<Cents>,
    pub tax_cents: Option<Cents>,
    pub total_cents: Option<Cents>,
    pub lines: Vec<Line>,
    pub notes: Option<String>,
}

/// One thing checked about a draft. A draft with every check passing can
/// become a bill as it stands; otherwise someone looks at the failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub check: String,
    pub ok: bool,
    pub detail: String,
}

fn check(out: &mut Vec<Check>, name: &str, ok: bool, detail: impl Into<String>) {
    out.push(Check {
        check: name.into(),
        ok,
        detail: detail.into(),
    });
}

fn clean(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn date(s: &Option<String>) -> Result<Option<NaiveDate>, String> {
    match clean(s) {
        None => Ok(None),
        Some(d) => NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| format!("\"{d}\" isn't a YYYY-MM-DD date")),
    }
}

fn dollars(c: Cents) -> String {
    let sign = if c < 0 { "-" } else { "" };
    format!("{sign}${}.{:02}", c.abs() / 100, c.abs() % 100)
}

/// Turns the model's answer into a draft and checks it.
pub fn interpret(read: &Read) -> (Draft, Vec<Check>) {
    let mut checks = Vec::new();
    fn money(name: &str, s: &Option<String>, checks: &mut Vec<Check>) -> Option<Cents> {
        let v = clean(s)?;
        parse_money(&v)
            .map_err(|e| check(checks, &format!("{name}_readable"), false, e))
            .ok()
    }
    let subtotal = money("subtotal", &read.subtotal, &mut checks);
    let tax = money("tax", &read.tax, &mut checks);
    let total = money("total", &read.total, &mut checks);
    let mut lines = Vec::new();
    let mut lines_ok = true;
    for (i, l) in read.lines.iter().enumerate() {
        match parse_money(&l.amount) {
            Ok(c) => lines.push(Line {
                description: l.description.trim().to_owned(),
                amount_cents: c,
                account: None,
            }),
            Err(e) => {
                lines_ok = false;
                check(
                    &mut checks,
                    "lines_readable",
                    false,
                    format!("line {}: {e}", i + 1),
                );
            }
        }
    }
    let invoice_date = date(&read.invoice_date).unwrap_or_else(|e| {
        check(&mut checks, "invoice_date_readable", false, e);
        None
    });
    let due_date = date(&read.due_date).unwrap_or_else(|e| {
        check(&mut checks, "due_date_readable", false, e);
        None
    });
    let draft = Draft {
        kind: read.kind,
        vendor_name: clean(&read.vendor_name),
        vendor_email: clean(&read.vendor_email).map(|e| e.to_lowercase()),
        account_number: clean(&read.account_number),
        invoice_number: clean(&read.invoice_number),
        invoice_date,
        due_date,
        currency: clean(&read.currency).map(|c| c.to_uppercase()),
        po_number: clean(&read.po_number),
        subtotal_cents: subtotal,
        tax_cents: tax,
        total_cents: total,
        lines,
        notes: clean(&read.notes),
    };

    let kind_name = serde_json::to_value(draft.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    check(
        &mut checks,
        "is_invoice",
        draft.kind == Kind::Invoice,
        match draft.kind {
            Kind::Invoice => "an invoice".to_owned(),
            Kind::Other => "this doesn't read as an invoice".to_owned(),
            _ => format!(
                "this reads as a {}, not an invoice to pay",
                kind_name.replace('_', " ")
            ),
        },
    );
    match draft.total_cents {
        Some(t) if t > 0 => check(&mut checks, "total", true, dollars(t)),
        Some(t) => check(
            &mut checks,
            "total",
            false,
            format!("the total is {}", dollars(t)),
        ),
        None if read.total.is_none() => check(&mut checks, "total", false, "no total found"),
        None => {}
    }
    check(
        &mut checks,
        "invoice_number",
        draft
            .invoice_number
            .as_deref()
            .is_some_and(|n| !invoice_key(n).is_empty()),
        draft
            .invoice_number
            .clone()
            .unwrap_or_else(|| "no invoice number found".into()),
    );
    match (draft.invoice_date, &read.invoice_date) {
        (Some(d), _) => check(&mut checks, "invoice_date", true, d.to_string()),
        (None, None) => check(&mut checks, "invoice_date", false, "no invoice date found"),
        (None, Some(_)) => {}
    }
    if let (Some(inv), Some(due)) = (draft.invoice_date, draft.due_date) {
        check(
            &mut checks,
            "due_after_invoice",
            due >= inv,
            if due >= inv {
                format!("due {due}")
            } else {
                format!("due {due} is before the invoice date {inv}")
            },
        );
    }
    match draft.currency.as_deref() {
        None | Some("USD") => {}
        Some(c) => check(
            &mut checks,
            "currency",
            false,
            format!("in {c}; Payvy handles US dollars only so far"),
        ),
    }
    if let (Some(t), true) = (draft.total_cents, lines_ok) {
        let sum: Cents = draft.lines.iter().map(|l| l.amount_cents).sum();
        let tax = draft.tax_cents.unwrap_or(0);
        if draft.lines.is_empty() {
            check(&mut checks, "lines_add_up", true, "no line items read");
        } else if sum == t || sum + tax == t {
            check(
                &mut checks,
                "lines_add_up",
                true,
                format!("lines {}", dollars(sum)),
            );
        } else {
            let with_tax = if tax != 0 {
                format!(" plus {} tax", dollars(tax))
            } else {
                String::new()
            };
            check(
                &mut checks,
                "lines_add_up",
                false,
                format!(
                    "the lines add up to {}{with_tax}, but the total is {}",
                    dollars(sum),
                    dollars(t)
                ),
            );
        }
    }
    if let (Some(s), Some(x), Some(t)) = (draft.subtotal_cents, draft.tax_cents, draft.total_cents)
    {
        check(
            &mut checks,
            "subtotal_plus_tax",
            s + x == t,
            format!(
                "{} + {} tax {} {}",
                dollars(s),
                dollars(x),
                if s + x == t { "=" } else { "≠" },
                dollars(t)
            ),
        );
    }
    (draft, checks)
}

/// The lines a bill can carry: positive, adding up to the total, with tax
/// as its own line. None when the draft's lines can't satisfy that (a
/// discount line, or lines that don't add up); the bill is then entered
/// without lines.
pub fn bill_lines(draft: &Draft) -> Option<Vec<Line>> {
    let total = draft.total_cents?;
    if draft.lines.is_empty() || draft.lines.iter().any(|l| l.amount_cents <= 0) {
        return None;
    }
    let sum: Cents = draft.lines.iter().map(|l| l.amount_cents).sum();
    let mut lines = draft.lines.clone();
    match draft.tax_cents {
        _ if sum == total => {}
        Some(tax) if tax > 0 && sum + tax == total => lines.push(Line {
            description: "Tax".into(),
            amount_cents: tax,
            account: None,
        }),
        _ => return None,
    }
    Some(lines)
}

// ── Vendor matching ─────────────────────────────────────────────────────────

/// A vendor already on file, for matching.
#[derive(Debug, Clone)]
pub struct Candidate<Id> {
    pub id: Id,
    pub name: String,
    pub email: Option<String>,
    pub account_numbers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// The name matches.
    Medium,
    /// An account number or the email domain matches.
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum VendorMatch<Id> {
    Matched {
        vendor_id: Id,
        name: String,
        confidence: Confidence,
        reasons: Vec<String>,
    },
    /// More than one vendor fits equally well; a person picks.
    Ambiguous { vendor_ids: Vec<Id> },
    /// No vendor on file fits; this is probably a new one.
    NoMatch,
}

/// Mail providers anyone can sign up to: sharing one says nothing about
/// being the same business.
const SHARED_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "yahoo.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "icloud.com",
    "me.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "msn.com",
    "comcast.net",
];

fn domain(email: &str) -> Option<String> {
    let d = email
        .rsplit_once('@')?
        .1
        .trim()
        .trim_end_matches('>')
        .to_lowercase();
    (!d.is_empty() && !SHARED_DOMAINS.contains(&d.as_str())).then_some(d)
}

/// A business name compared loosely: case, punctuation and the legal
/// suffix don't matter, so "King Environmental, LLC" is "king environmental".
pub fn name_key(name: &str) -> String {
    const SUFFIXES: &[&str] = &[
        "inc",
        "incorporated",
        "llc",
        "l l c",
        "ltd",
        "limited",
        "co",
        "corp",
        "corporation",
        "company",
        "lp",
        "llp",
        "pllc",
        "pc",
    ];
    let mut words: Vec<String> = name
        .to_lowercase()
        .replace('&', " and ")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();
    if words.first().is_some_and(|w| w == "the") {
        words.remove(0);
    }
    while words.len() > 1 && words.last().is_some_and(|w| SUFFIXES.contains(&w.as_str())) {
        words.pop();
    }
    words.join(" ")
}

/// Finds the vendor a draft is from. `sender` is the address the document
/// was emailed from, when it came by email.
pub fn match_vendor<Id: Clone + PartialEq>(
    draft: &Draft,
    sender: Option<&str>,
    candidates: &[Candidate<Id>],
) -> VendorMatch<Id> {
    let account = draft
        .account_number
        .as_deref()
        .map(invoice_key)
        .filter(|k| !k.is_empty());
    let domains: Vec<String> = [draft.vendor_email.as_deref(), sender]
        .into_iter()
        .flatten()
        .filter_map(domain)
        .collect();
    let name = draft
        .vendor_name
        .as_deref()
        .map(name_key)
        .filter(|k| !k.is_empty());

    let mut best: Vec<(Confidence, &Candidate<Id>, Vec<String>)> = Vec::new();
    for c in candidates {
        let mut reasons = Vec::new();
        let mut confidence = None;
        if let Some(a) = &account
            && c.account_numbers.iter().any(|n| invoice_key(n) == *a)
        {
            reasons.push(format!(
                "account number {} is on file for {}",
                draft.account_number.as_deref().unwrap_or_default(),
                c.name
            ));
            confidence = Some(Confidence::High);
        }
        if let Some(d) = c.email.as_deref().and_then(domain)
            && domains.contains(&d)
        {
            reasons.push(format!("email from {d}, the domain on file for {}", c.name));
            confidence = Some(Confidence::High);
        }
        if name.as_deref().is_some_and(|n| n == name_key(&c.name)) {
            reasons.push(format!("the name matches {}", c.name));
            confidence = confidence.or(Some(Confidence::Medium));
        }
        if let Some(conf) = confidence {
            best.push((conf, c, reasons));
        }
    }
    let Some(top) = best.iter().map(|(c, _, _)| *c).max() else {
        return VendorMatch::NoMatch;
    };
    let mut top: Vec<_> = best.into_iter().filter(|(c, _, _)| *c == top).collect();
    if top.len() > 1 {
        // An account number beats an email domain: a vendor with several
        // entities on one domain still has one account number per entity.
        if let Some(a) = &account {
            top.retain(|(_, c, _)| c.account_numbers.iter().any(|n| invoice_key(n) == *a));
        }
    }
    match top.len() {
        1 => {
            let (confidence, c, reasons) = top.remove(0);
            VendorMatch::Matched {
                vendor_id: c.id.clone(),
                name: c.name.clone(),
                confidence,
                reasons,
            }
        }
        0 => VendorMatch::NoMatch,
        _ => VendorMatch::Ambiguous {
            vendor_ids: top.into_iter().map(|(_, c, _)| c.id.clone()).collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.into())
    }

    fn invoice() -> Read {
        Read {
            kind: Kind::Invoice,
            vendor_name: s("King Environmental, LLC"),
            vendor_email: s("AR@KingEnv.com"),
            account_number: s("004417"),
            invoice_number: s("189727"),
            invoice_date: s("2026-11-01"),
            due_date: s("2026-12-01"),
            currency: s("USD"),
            subtotal: s("425.00"),
            tax: s("34.00"),
            total: s("$459.00"),
            lines: vec![
                ReadLine {
                    description: "30 yd roll-off haul".into(),
                    amount: "300.00".into(),
                },
                ReadLine {
                    description: "Disposal, 2.5 tons".into(),
                    amount: "125.00".into(),
                },
            ],
            ..Read::default()
        }
    }

    #[test]
    fn money_is_read_exactly_or_not_at_all() {
        assert_eq!(parse_money("1,234.56"), Ok(123_456));
        assert_eq!(parse_money("$1,234.5"), Ok(123_450));
        assert_eq!(parse_money("12"), Ok(1_200));
        assert_eq!(parse_money("-12.00"), Ok(-1_200));
        assert_eq!(parse_money("(12.00)"), Ok(-1_200));
        assert_eq!(parse_money("$ 0.99 USD"), Ok(99));
        assert_eq!(parse_money("1,000,000.00"), Ok(100_000_000));
        assert!(parse_money("12.345").is_err());
        assert!(parse_money("1.234,56").is_err());
        assert!(parse_money("12,34.00").is_err());
        assert!(parse_money("about 40").is_err());
        assert!(parse_money("").is_err());
        assert!(parse_money("99999999999999999999").is_err());
    }

    #[test]
    fn a_clean_invoice_passes_every_check() {
        let (draft, checks) = interpret(&invoice());
        assert!(checks.iter().all(|c| c.ok), "{checks:#?}");
        assert_eq!(draft.total_cents, Some(45_900));
        assert_eq!(draft.vendor_email.as_deref(), Some("ar@kingenv.com"));
        let lines = bill_lines(&draft).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].description, "Tax");
        assert_eq!(lines.iter().map(|l| l.amount_cents).sum::<i64>(), 45_900);
    }

    #[test]
    fn disagreeing_numbers_fail_their_checks() {
        let mut r = invoice();
        r.total = s("495.00");
        r.due_date = s("2026-10-01");
        r.kind = Kind::Statement;
        let (draft, checks) = interpret(&r);
        let failed: Vec<&str> = checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| c.check.as_str())
            .collect();
        assert_eq!(
            failed,
            [
                "is_invoice",
                "due_after_invoice",
                "lines_add_up",
                "subtotal_plus_tax"
            ]
        );
        assert_eq!(bill_lines(&draft), None);

        let mut r = invoice();
        r.total = s("about $460");
        r.invoice_number = None;
        r.lines[0].amount = "3OO.00".into();
        let (_, checks) = interpret(&r);
        let failed: Vec<&str> = checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| c.check.as_str())
            .collect();
        assert_eq!(
            failed,
            ["total_readable", "lines_readable", "invoice_number"]
        );
    }

    #[test]
    fn discount_lines_keep_the_bill_but_not_its_lines() {
        let mut r = invoice();
        r.tax = None;
        r.subtotal = None;
        r.total = s("400.00");
        r.lines.push(ReadLine {
            description: "Loyalty discount".into(),
            amount: "-25.00".into(),
        });
        let (draft, checks) = interpret(&r);
        assert!(checks.iter().all(|c| c.ok), "{checks:#?}");
        assert_eq!(bill_lines(&draft), None);
    }

    #[test]
    fn the_schema_is_strict_everywhere() {
        let v = schema();
        let props = v["properties"].as_object().unwrap();
        assert_eq!(v["required"].as_array().unwrap().len(), props.len());
        assert_eq!(v["additionalProperties"], false);
        assert_eq!(
            v["properties"]["lines"]["items"]["additionalProperties"],
            false
        );
        // Every field of Read is in the schema, and a full answer parses.
        let filled: serde_json::Map<String, Value> = props
            .keys()
            .map(|k| (k.clone(), Value::Null))
            .chain([
                ("kind".into(), json!("invoice")),
                ("lines".into(), json!([])),
            ])
            .collect();
        let read: Read = serde_json::from_value(Value::Object(filled)).unwrap();
        let back = serde_json::to_value(&read).unwrap();
        assert_eq!(back.as_object().unwrap().len(), props.len());
    }

    fn vendors() -> Vec<Candidate<u32>> {
        vec![
            Candidate {
                id: 1,
                name: "King Environmental".into(),
                email: s("billing@kingenv.com"),
                account_numbers: vec![],
            },
            Candidate {
                id: 2,
                name: "Acme Supply Co.".into(),
                email: s("acme.supply@gmail.com"),
                account_numbers: vec!["4417".into()],
            },
            Candidate {
                id: 3,
                name: "Acme Supply".into(),
                email: None,
                account_numbers: vec![],
            },
        ]
    }

    #[test]
    fn vendors_match_by_account_then_domain_then_name() {
        let (mut draft, _) = interpret(&invoice());
        // Account number 004417 is Acme's; the email domain is King's. Both
        // are strong, so the account number decides.
        match match_vendor(&draft, None, &vendors()) {
            VendorMatch::Matched {
                vendor_id,
                confidence,
                ..
            } => {
                assert_eq!((vendor_id, confidence), (2, Confidence::High))
            }
            other => panic!("{other:?}"),
        }
        draft.account_number = None;
        assert!(matches!(
            match_vendor(&draft, None, &vendors()),
            VendorMatch::Matched {
                vendor_id: 1,
                confidence: Confidence::High,
                ..
            }
        ));
        // A shared mail provider proves nothing; the name alone is medium.
        draft.vendor_email = s("someone@gmail.com");
        assert!(matches!(
            match_vendor(&draft, Some("acme.supply@gmail.com"), &vendors()),
            VendorMatch::Matched {
                vendor_id: 1,
                confidence: Confidence::Medium,
                ..
            }
        ));
        draft.vendor_name = s("ACME SUPPLY, INC.");
        assert_eq!(
            match_vendor(&draft, None, &vendors()),
            VendorMatch::Ambiguous {
                vendor_ids: vec![2, 3]
            }
        );
        draft.vendor_name = s("Unknown Hauling");
        assert_eq!(match_vendor(&draft, None, &vendors()), VendorMatch::NoMatch);
    }

    #[test]
    fn name_keys_drop_case_punctuation_and_suffixes() {
        assert_eq!(name_key("King Environmental, LLC"), "king environmental");
        assert_eq!(name_key("The Home Depot"), "home depot");
        assert_eq!(name_key("Smith & Sons Co."), "smith and sons");
        assert_eq!(name_key("Co"), "co");
    }
}
