//! Bill rules that don't depend on storage: what a valid bill is, and how
//! invoice numbers are compared for duplicate detection.

use serde::{Deserialize, Serialize};

use crate::Cents;

/// A bill's lifecycle outside its workflow. While `open`, where it stands is
/// derived by `workflow::evaluate`; the other states are final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    Rejected,
    Void,
    Paid,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Rejected => "rejected",
            State::Void => "void",
            State::Paid => "paid",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => State::Open,
            "rejected" => State::Rejected,
            "void" => State::Void,
            "paid" => State::Paid,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub description: String,
    pub amount_cents: Cents,
    /// The expense account in the books it posts to, e.g. "6100 Repairs".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Quantity as printed/entered ("2.5"), for quantity matching against
    /// PO lines. Absent: the line is amount-only and is not quantity-checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_price_cents: Option<Cents>,
}

impl Line {
    pub fn amount(description: String, amount_cents: Cents) -> Self {
        Line {
            description,
            amount_cents,
            account: None,
            quantity: None,
            unit: None,
            unit_price_cents: None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BillError {
    #[error("amount_cents must be more than zero")]
    NotPositive,
    #[error("invoice_number can't be empty")]
    NoInvoiceNumber,
    #[error("line amounts exceed the supported range")]
    AmountOverflow,
    #[error("the lines add up to {lines} cents but the bill is {total} cents")]
    LinesDontAddUp { lines: Cents, total: Cents },
    #[error("due_date is before invoice_date")]
    DueBeforeInvoice,
}

/// All bills may contain signed adjustments and zero rows. A nonempty breakdown
/// must reconcile exactly to a positive total, without integer overflow.
pub fn validate(
    amount_cents: Cents,
    invoice_number: &str,
    lines: &[Line],
    invoice_date: chrono::NaiveDate,
    due_date: Option<chrono::NaiveDate>,
) -> Result<(), BillError> {
    if amount_cents <= 0 {
        return Err(BillError::NotPositive);
    }
    if invoice_key(invoice_number).is_empty() {
        return Err(BillError::NoInvoiceNumber);
    }
    if !lines.is_empty() {
        let sum = lines
            .iter()
            .try_fold(0i64, |s, l| s.checked_add(l.amount_cents))
            .ok_or(BillError::AmountOverflow)?;
        if sum != amount_cents {
            return Err(BillError::LinesDontAddUp {
                lines: sum,
                total: amount_cents,
            });
        }
    }
    if due_date.is_some_and(|d| d < invoice_date) {
        return Err(BillError::DueBeforeInvoice);
    }
    Ok(())
}

/// What a later copy of an invoice is, against the amount the bill
/// currently carries: the same amount again (a resend, recorded and never
/// payable twice) or a different one (a revision a person must decide).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrivalKind {
    Resend,
    Revision,
}

pub fn arrival_kind(current_cents: Cents, arrived_cents: Cents) -> ArrivalKind {
    if arrived_cents == current_cents {
        ArrivalKind::Resend
    } else {
        ArrivalKind::Revision
    }
}

/// The form of an invoice number used to spot duplicates from one vendor:
/// case, spaces, punctuation and leading zeros don't make a different
/// invoice, so "INV-00042" and "inv 42" match. Letters do count: "INV-42"
/// and "42" are different invoices.
pub fn invoice_key(number: &str) -> String {
    // Drop zeros that lead a run of digits, where runs are split by letters
    // or punctuation: INV-00042 -> INV42, A10-020 -> A1020, 0042 -> 42.
    let mut out = String::with_capacity(number.len());
    let mut run_start = true;
    let mut dropped_zero = false;
    for c in number.chars() {
        if !c.is_alphanumeric() {
            run_start = true;
            continue;
        }
        if c.is_ascii_digit() {
            if run_start && c == '0' {
                dropped_zero = true;
                continue;
            }
            run_start = false;
        } else {
            run_start = true;
        }
        out.extend(c.to_uppercase());
    }
    if out.is_empty() && dropped_zero {
        "0".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn a_copy_is_a_resend_only_at_the_bills_current_amount() {
        assert_eq!(arrival_kind(8_740, 8_740), ArrivalKind::Resend);
        assert_eq!(arrival_kind(8_740, 3_749), ArrivalKind::Revision);
        assert_eq!(arrival_kind(3_749, 8_740), ArrivalKind::Revision);
    }

    #[test]
    fn invoice_keys_ignore_formatting_but_not_letters() {
        assert_eq!(invoice_key("INV-00042"), "INV42");
        assert_eq!(invoice_key("inv 42"), "INV42");
        assert_eq!(invoice_key("#0042"), "42");
        assert_ne!(invoice_key("INV-42"), invoice_key("42"));
        assert_eq!(invoice_key("000"), "0");
        assert_eq!(invoice_key(" - "), "");
        assert_eq!(invoice_key("A10-020"), invoice_key("A10-20"));
    }

    #[test]
    fn signed_and_zero_lines_reconcile_but_overflow_is_refused() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let lines = |amounts: &[i64]| {
            amounts
                .iter()
                .map(|&amount_cents| Line::amount("Printed line".into(), amount_cents))
                .collect::<Vec<_>>()
        };
        assert!(validate(9000, "SIGNED", &lines(&[10000, -1000, 0]), d, None).is_ok());
        assert_eq!(
            validate(1, "OVERFLOW", &lines(&[i64::MAX, 1, -i64::MAX]), d, None),
            Err(BillError::AmountOverflow)
        );
        assert_eq!(
            validate(0, "ZERO", &lines(&[1000, -1000]), d, None),
            Err(BillError::NotPositive)
        );
    }

    #[test]
    fn lines_must_add_up() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let lines = vec![
            Line::amount("Haul".into(), 30_000),
            Line::amount("Disposal".into(), 12_500),
        ];
        assert!(validate(42_500, "A1", &lines, d, None).is_ok());
        assert_eq!(
            validate(42_000, "A1", &lines, d, None),
            Err(BillError::LinesDontAddUp {
                lines: 42_500,
                total: 42_000
            })
        );
        assert_eq!(validate(0, "A1", &[], d, None), Err(BillError::NotPositive));
        assert_eq!(
            validate(1, "--", &[], d, None),
            Err(BillError::NoInvoiceNumber)
        );
        assert_eq!(
            validate(1, "A1", &[], d, d.pred_opt()),
            Err(BillError::DueBeforeInvoice)
        );
    }
}
