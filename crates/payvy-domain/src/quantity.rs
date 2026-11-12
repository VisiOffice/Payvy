//! Quantity matching (PO Matching Spec v2, roadmap 1): ordered vs billed
//! quantities and rates per line, cumulative across invoices. Pure: the
//! server supplies the lines and the sums follow. Quantities are exact
//! micro-units (`invoice_evidence::quantity_micro`); no float ever enters.

use serde::Serialize;

use crate::Cents;
use crate::bills::{Line, invoice_key};
use crate::invoice_evidence::quantity_micro;

/// One PO line that carries a quantity, as ordered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedLine {
    pub description: String,
    pub unit: Option<String>,
    pub ordered_micro: i64,
    pub unit_price_cents: Option<Cents>,
}

/// The findings one bill's lines raise against one PO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    /// Billed (this bill plus every earlier allocation) exceeds ordered.
    QuantityOver {
        description: String,
        unit: Option<String>,
        ordered_micro: i64,
        billed_before_micro: i64,
        this_bill_micro: i64,
    },
    /// The billed rate is not the PO line's rate, to the cent.
    RateMismatch {
        description: String,
        ordered_cents: Cents,
        billed_cents: Cents,
    },
    /// No PO line matches this bill line's description (and unit).
    UnmatchedLine { description: String },
    /// More than one PO line matches; a person must say which.
    AmbiguousLine { description: String },
}

impl Finding {
    pub fn kind(&self) -> &'static str {
        match self {
            Finding::QuantityOver { .. } => "quantity_over",
            Finding::RateMismatch { .. } => "rate_mismatch",
            Finding::UnmatchedLine { .. } => "unmatched_line",
            Finding::AmbiguousLine { .. } => "ambiguous_line",
        }
    }

    /// Whether this finding is about spend authority (it can block under
    /// an enforcing policy), as opposed to identification.
    pub fn blocks(&self) -> bool {
        matches!(
            self,
            Finding::QuantityOver { .. } | Finding::RateMismatch { .. }
        )
    }
}

fn unit_key(unit: &Option<String>) -> Option<String> {
    unit.as_ref()
        .map(|u| u.trim().to_lowercase())
        .filter(|u| !u.is_empty())
}

/// The ordered line a bill line belongs to: normalized description, with
/// the unit agreeing when both sides state one. None when nothing fits;
/// `ambiguous` when more than one line fits equally.
fn matches<'a>(ordered: &'a [OrderedLine], line: &Line) -> Option<Option<&'a OrderedLine>> {
    let key = invoice_key(&line.description);
    // The unit must agree when both sides state one: a bill line in
    // "each" never silently matches a PO line in "load". A PO line that
    // states no unit matches any unit.
    let candidates: Vec<&OrderedLine> = ordered
        .iter()
        .filter(|o| invoice_key(&o.description) == key)
        .filter(|o| {
            unit_key(&line.unit).is_none()
                || unit_key(&o.unit).is_none()
                || unit_key(&o.unit) == unit_key(&line.unit)
        })
        .collect();
    match candidates.len() {
        0 => None,
        1 => Some(Some(candidates[0])),
        _ => Some(None),
    }
}

fn quantity_of(line: &Line) -> Option<i64> {
    line.quantity.as_deref().and_then(quantity_micro)
}

/// A bill line's quantity scaled by a share (allocated ÷ bill amount),
/// for a bill split across several POs: only the share of its quantity
/// that this PO is paying for counts against this PO. Exact micro-unit
/// arithmetic; a line without a quantity passes through unchanged.
pub fn scaled(line: &Line, share_numerator: i64, share_denominator: i64) -> Line {
    let mut out = line.clone();
    if share_denominator > 0
        && let Some(q) = quantity_of(line)
    {
        let scaled = (q as i128 * share_numerator as i128 / share_denominator as i128) as i64;
        out.quantity = Some(crate::invoice_evidence::format_micro(scaled));
    }
    out
}

/// What one bill's lines raise against one PO: cumulative quantity over
/// the ordered amount (counting every earlier bill's matched lines), rate
/// differences to the cent, and lines that cannot be identified. Lines
/// without quantities, on either side, are not quantity-checked.
pub fn evaluate(ordered: &[OrderedLine], prior: &[Line], current: &[Line]) -> Vec<Finding> {
    let mut findings = Vec::new();
    // Cumulative billed per ordered line, from earlier bills.
    let mut billed: Vec<i64> = vec![0; ordered.len()];
    for line in prior {
        if let Some(Some(o)) = matches(ordered, line)
            && let Some(q) = quantity_of(line)
        {
            let idx = ordered.iter().position(|x| std::ptr::eq(x, o)).unwrap();
            billed[idx] += q;
        }
    }
    let mut this_bill: Vec<i64> = vec![0; ordered.len()];
    for line in current {
        if quantity_of(line).is_none() && line.unit_price_cents.is_none() {
            continue; // an amount-only line: nothing to quantity-check
        }
        match matches(ordered, line) {
            None => findings.push(Finding::UnmatchedLine {
                description: line.description.clone(),
            }),
            Some(None) => findings.push(Finding::AmbiguousLine {
                description: line.description.clone(),
            }),
            Some(Some(o)) => {
                let idx = ordered.iter().position(|x| std::ptr::eq(x, o)).unwrap();
                if let Some(q) = quantity_of(line) {
                    this_bill[idx] += q;
                }
                if let (Some(ordered_rate), Some(billed_rate)) =
                    (o.unit_price_cents, line.unit_price_cents)
                    && ordered_rate != billed_rate
                {
                    findings.push(Finding::RateMismatch {
                        description: line.description.clone(),
                        ordered_cents: ordered_rate,
                        billed_cents: billed_rate,
                    });
                }
            }
        }
    }
    for (i, o) in ordered.iter().enumerate() {
        if this_bill[i] > 0 && billed[i] + this_bill[i] > o.ordered_micro {
            findings.push(Finding::QuantityOver {
                description: o.description.clone(),
                unit: o.unit.clone(),
                ordered_micro: o.ordered_micro,
                billed_before_micro: billed[i],
                this_bill_micro: this_bill[i],
            });
        }
    }
    findings
}

/// Total billed quantity per ordered line from one set of bill lines,
/// aligned with `ordered` (0 where a line matched nothing or matched
/// ambiguously — identification findings are `evaluate`'s job).
pub fn billed_micro(ordered: &[OrderedLine], lines: &[Line]) -> Vec<i64> {
    let mut out = vec![0; ordered.len()];
    for line in lines {
        if let Some(Some(o)) = matches(ordered, line)
            && let Some(q) = quantity_of(line)
        {
            let idx = ordered.iter().position(|x| std::ptr::eq(x, o)).unwrap();
            out[idx] += q;
        }
    }
    out
}

/// Ordered / billed-before / remaining for one ordered line, for display:
/// remaining never goes below zero.
pub fn remaining_micro(ordered: i64, billed_before: i64, this_bill: i64) -> i64 {
    (ordered - billed_before - this_bill).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(description: &str, quantity: Option<&str>, rate: Option<Cents>) -> Line {
        Line {
            description: description.into(),
            amount_cents: 0,
            account: None,
            quantity: quantity.map(str::to_owned),
            unit: None,
            unit_price_cents: rate,
        }
    }

    fn ordered(description: &str, micro: i64, rate: Option<Cents>) -> OrderedLine {
        OrderedLine {
            description: description.into(),
            unit: None,
            ordered_micro: micro,
            unit_price_cents: rate,
        }
    }

    #[test]
    fn cumulative_quantities_across_invoices_and_exact_rates() {
        let po = vec![ordered("Haul, 30 yd", 3_000_000, Some(30_000))];
        // Two earlier invoices billed 2 of 3; this one bills 2 more.
        let prior = vec![line("haul 30 YD", Some("2"), Some(30_000))];
        let now = vec![line("Haul, 30 yd", Some("2"), Some(30_000))];
        let f = evaluate(&po, &prior, &now);
        assert_eq!(f.len(), 1);
        match &f[0] {
            Finding::QuantityOver {
                billed_before_micro,
                this_bill_micro,
                ..
            } => {
                assert_eq!(*billed_before_micro, 2_000_000);
                assert_eq!(*this_bill_micro, 2_000_000);
            }
            other => panic!("{other:?}"),
        }
        // Within the ordered quantity, at the ordered rate: no findings.
        let ok = vec![line("Haul, 30 yd", Some("1"), Some(30_000))];
        assert!(evaluate(&po, &prior, &ok).is_empty());
        // One cent over the rate is a mismatch.
        let rate = vec![line("Haul, 30 yd", Some("1"), Some(30_001))];
        assert_eq!(evaluate(&po, &[], &rate)[0].kind(), "rate_mismatch");
    }

    #[test]
    fn unmatched_and_ambiguous_lines_are_findings_not_mappings() {
        let po = vec![
            ordered("Disposal", 1_000_000, None),
            ordered("Disposal", 2_000_000, None),
        ];
        let f = evaluate(&po, &[], &[line("Disposal", Some("1"), None)]);
        assert_eq!(f[0].kind(), "ambiguous_line");
        let f = evaluate(&po, &[], &[line("What is this", Some("1"), None)]);
        assert_eq!(f[0].kind(), "unmatched_line");
        // Amount-only lines are not quantity-checked at all.
        assert!(evaluate(&po, &[], &[line("What is this", None, None)]).is_empty());
    }
}
