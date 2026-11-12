//! Durable preparation, separate from immutable model output and bill approval.
use crate::{bills::Line, extraction::parse_money};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub id: String,
    pub description: String,
    /// As typed, including unreadable/empty values while preparing.
    pub amount: String,
    pub source_index: Option<usize>,
    /// Exclusion retains the row and requires a reason.
    pub excluded_reason: Option<String>,
    #[serde(default)]
    pub details: Option<crate::invoice_evidence::LineDetails>,
    #[serde(default)]
    pub quantity_resolution: Option<String>,
    #[serde(default)]
    pub source_resolution: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaxBasis {
    Included,
    Additional,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub vendor_id: Option<String>,
    pub invoice_number: String,
    pub invoice_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub total: String,
    pub currency: String,
    pub memo: String,
    pub rows: Vec<Row>,
    pub tax: String,
    pub tax_basis: TaxBasis,
    /// A review of the original, not a model's accuracy claim.
    pub source_checked: bool,
    #[serde(default)]
    pub coverage_resolution: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub code: &'static str,
    pub row_id: Option<String>,
    pub detail: String,
}
#[derive(Debug, Serialize)]
pub struct Validation {
    pub ready: bool,
    pub header_ready: bool,
    pub total_cents: Option<i64>,
    pub sum_cents: Option<i64>,
    pub difference_cents: Option<i64>,
    pub issues: Vec<Issue>,
    pub lines: Vec<Line>,
}

/// Structural limits protect persistence as well as bill creation. Financial
/// mistakes remain saveable; malformed identifiers/lineage do not.
pub fn check_shape(r: &Review) -> Result<(), String> {
    if r.rows.len() > 1000
        || r.invoice_number.len() > 500
        || r.memo.len() > 4000
        || r.currency.len() > 10
        || r.total.len() > 100
        || r.tax.len() > 100
        || r.coverage_resolution
            .as_ref()
            .is_some_and(|s| s.len() > 1000)
    {
        return Err("Review exceeds field or row limits".into());
    }
    let mut ids = HashSet::new();
    let mut sources = HashSet::new();
    for row in &r.rows {
        if row.id.is_empty()
            || row.id.len() > 100
            || !ids.insert(&row.id)
            || row.description.len() > 4000
            || row.amount.len() > 100
            || row.details.as_ref().is_some_and(|d| !d.bounded())
            || row
                .quantity_resolution
                .as_ref()
                .is_some_and(|s| s.len() > 1000)
            || row
                .source_resolution
                .as_ref()
                .is_some_and(|s| s.len() > 1000)
            || row
                .excluded_reason
                .as_ref()
                .is_some_and(|s| s.trim().is_empty() || s.len() > 1000)
            || row.source_index.is_some_and(|i| !sources.insert(i))
        {
            return Err(
                "Rows need unique IDs/source indices, bounded fields and a reason when excluded"
                    .into(),
            );
        }
    }
    Ok(())
}

pub fn validate(r: &Review) -> Validation {
    let mut issues = Vec::new();
    let mut issue = |code, row_id, detail: &str| {
        issues.push(Issue {
            code,
            row_id,
            detail: detail.into(),
        })
    };
    let total = parse_money(&r.total).ok().filter(|v| *v > 0);
    if total.is_none() {
        issue("total_unreadable", None, "Enter a positive invoice total.");
    }
    if r.currency != "USD" {
        issue(
            "currency_unresolved",
            None,
            "Confirm USD from the original. Other currencies are not supported.",
        );
    }
    if r.invoice_number.trim().is_empty() {
        issue("invoice_number_missing", None, "Enter the invoice number.");
    }
    if r.invoice_date.is_none() {
        issue("invoice_date_missing", None, "Enter the invoice date.");
    }
    if r.vendor_id.as_ref().is_none_or(|s| s.is_empty()) {
        issue("vendor_missing", None, "Choose a vendor.");
    }
    if r.due_date
        .zip(r.invoice_date)
        .is_some_and(|(due, date)| due < date)
    {
        issue(
            "due_before_invoice",
            None,
            "Due date is before invoice date.",
        );
    }
    if !r.source_checked {
        issue(
            "source_unverified",
            None,
            "Check the invoice, currency and selected pages against the original.",
        );
    }
    let header_ready = issues.is_empty();
    let mut lines = Vec::new();
    let mut sum = Some(0i64);
    for row in r.rows.iter().filter(|r| r.excluded_reason.is_none()) {
        if let Some(details) = &row.details {
            if let (Some(q), Some(rate)) = (&details.quantity, &details.unit_price) {
                let calculated = crate::invoice_evidence::product_cents(q, rate);
                let printed = parse_money(&row.amount).ok();
                if (calculated.is_none() || calculated != printed)
                    && row
                        .quantity_resolution
                        .as_ref()
                        .is_none_or(|s| s.trim().is_empty())
                {
                    issues.push(Issue{code:"quantity_amount_mismatch",row_id:Some(row.id.clone()),detail:"Quantity × rate differs from the printed amount (USD rounded to cents, ties away from zero). Correct the fields or explain the printed discount, tax, bundle or rounding; the amount is never rewritten.".into()});
                }
            }
            let mut values = vec![
                ("description", Some(row.description.as_str())),
                ("amount", Some(row.amount.as_str())),
            ];
            values.extend(details.fields());
            for (field, value) in values {
                use crate::invoice_evidence::FieldState;
                let state = details.states.get(field);
                let present = value.is_some_and(|v| !v.trim().is_empty());
                let uncertain = matches!(
                    state,
                    Some(FieldState::Unreadable | FieldState::Conflicting)
                ) || matches!(state, Some(FieldState::Present)) && !present
                    || matches!(state, Some(FieldState::Absent)) && present;
                if uncertain
                    && row
                        .source_resolution
                        .as_ref()
                        .is_none_or(|s| s.trim().is_empty())
                {
                    issues.push(Issue{code:"field_unresolved",row_id:Some(row.id.clone()),detail:format!("{field} is unreadable, conflicting or disagrees with its recorded state. Check it against the original and explain the resolution.")});
                }
            }
        }

        if row.description.trim().is_empty() {
            issues.push(Issue {
                code: "description_missing",
                row_id: Some(row.id.clone()),
                detail: "Enter a description or exclude this row with a reason.".into(),
            });
        }
        match parse_money(&row.amount) {
            Ok(amount_cents) => {
                sum = sum.and_then(|s| s.checked_add(amount_cents));
                lines.push(Line {
                    description: row.description.trim().into(),
                    amount_cents,
                    account: None,
                    quantity: row
                        .details
                        .as_ref()
                        .and_then(|d| d.quantity.clone())
                        .filter(|q| crate::invoice_evidence::quantity_micro(q).is_some()),
                    unit: row.details.as_ref().and_then(|d| d.unit.clone()),
                    unit_price_cents: row
                        .details
                        .as_ref()
                        .and_then(|d| d.unit_price.as_deref())
                        .and_then(|p| parse_money(p).ok()),
                });
            }
            Err(_) => {
                sum = None;
                issues.push(Issue {
                    code: "amount_unreadable",
                    row_id: Some(row.id.clone()),
                    detail: "Enter the printed amount; an empty amount is not zero.".into(),
                });
            }
        }
    }
    if lines.is_empty() {
        issues.push(Issue {
            code: "lines_missing",
            row_id: None,
            detail: "No line items captured. Add rows or explicitly accept a summary-only bill."
                .into(),
        });
    }
    match r.tax_basis {
        TaxBasis::Unknown => issues.push(Issue {
            code: "tax_basis_unknown",
            row_id: None,
            detail: "Choose whether tax is included in the rows or added once.".into(),
        }),
        TaxBasis::Additional => match parse_money(&r.tax) {
            Ok(tax) if tax >= 0 => {
                sum = sum.and_then(|s| s.checked_add(tax));
                if tax > 0 {
                    lines.push(Line::amount("Tax".into(), tax));
                }
            }
            _ => {
                sum = None;
                issues.push(Issue {
                    code: "tax_unreadable",
                    row_id: None,
                    detail: "Enter the separately charged tax amount.".into(),
                });
            }
        },
        TaxBasis::Included => {}
    }
    let difference = total
        .zip(sum)
        .and_then(|(total, sum)| total.checked_sub(sum));
    if difference != Some(0) {
        issues.push(Issue { code: "total_mismatch", row_id: None, detail: "Rows and additional tax must equal the invoice total exactly. No balancing row will be added.".into() });
    }
    Validation {
        ready: issues.is_empty(),
        header_ready,
        total_cents: total,
        sum_cents: sum,
        difference_cents: difference,
        issues,
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Review {
        Review {
            coverage_resolution: None,
            vendor_id: Some("vendor".into()),
            invoice_number: "A1".into(),
            invoice_date: NaiveDate::from_ymd_opt(2026, 11, 1),
            due_date: None,
            total: "90.00".into(),
            currency: "USD".into(),
            memo: String::new(),
            rows: vec![
                Row {
                    details: None,
                    quantity_resolution: None,
                    source_resolution: None,
                    id: "a".into(),
                    description: "Service".into(),
                    amount: "100.00".into(),
                    source_index: Some(0),
                    excluded_reason: None,
                },
                Row {
                    details: None,
                    quantity_resolution: None,
                    source_resolution: None,
                    id: "b".into(),
                    description: "Discount".into(),
                    amount: "-10.00".into(),
                    source_index: Some(1),
                    excluded_reason: None,
                },
            ],
            tax: String::new(),
            tax_basis: TaxBasis::Included,
            source_checked: true,
        }
    }
    #[test]
    fn discounts_and_zero_rows_survive() {
        let mut r = sample();
        r.rows.push(Row {
            details: None,
            quantity_resolution: None,
            source_resolution: None,
            id: "z".into(),
            description: "Included".into(),
            amount: "0".into(),
            source_index: None,
            excluded_reason: None,
        });
        let v = validate(&r);
        assert!(v.ready);
        assert_eq!(v.lines.len(), 3);
        assert_eq!(v.lines[1].amount_cents, -1000);
    }
    #[test]
    fn missing_amount_is_not_zero_and_totals_do_not_remove_rows() {
        let mut r = sample();
        r.total = "100.00".into();
        let v = validate(&r);
        assert!(!v.ready);
        assert_eq!(v.difference_cents, Some(1000));
        assert_eq!(v.lines.len(), 2);
        r.rows[1].amount.clear();
        assert!(!validate(&r).ready);
        assert!(validate(&r).sum_cents.is_none());
        r.rows.clear();
        assert!(
            validate(&r)
                .issues
                .iter()
                .any(|i| i.code == "lines_missing")
        );
    }
    #[test]
    fn tax_is_an_explicit_component() {
        let mut r = sample();
        r.total = "99.00".into();
        r.tax = "9.00".into();
        r.tax_basis = TaxBasis::Additional;
        assert!(validate(&r).ready);
        r.tax_basis = TaxBasis::Included;
        assert!(!validate(&r).ready);
        r.tax_basis = TaxBasis::Unknown;
        assert!(!validate(&r).ready);
    }
    #[test]
    fn evidence_and_shape_are_checked() {
        let mut r = sample();
        r.source_checked = false;
        assert!(!validate(&r).header_ready);
        r.rows.push(r.rows[0].clone());
        assert!(check_shape(&r).is_err());
    }
}
