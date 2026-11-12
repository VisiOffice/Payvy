//! PO matching amounts and progress (PO Matching Spec v2). Pure: the server
//! supplies the allocation sums read from the ledger.

use crate::Cents;

/// The overage allowance in cents for one PO revision: the smaller of the
/// configured dollar and percentage caps. An absent cap is "not configured"
/// and contributes nothing; a configured zero means zero. With neither cap
/// configured the allowance is 0 (zero overage by default).
pub fn allowance_cents(
    authorized_cents: Cents,
    dollar_cap: Option<Cents>,
    percent_bp: Option<i64>,
) -> Cents {
    let percent = percent_bp.map(|bp| authorized_cents.saturating_mul(bp) / 10_000);
    match (dollar_cap, percent) {
        (Some(d), Some(p)) => d.min(p),
        (Some(d), None) => d,
        (None, Some(p)) => p,
        (None, None) => 0,
    }
}

/// The enumerated vendor/company policies (the registry): a small set of
/// named policies with typed options, evaluated by the server as pure
/// decisions. A person sets them; they are versioned and pinned to bills
/// like workflows. Agents may only propose changes (a later build).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverPeriod {
    /// Show it beside the bill; it never blocks payment (the default).
    FlagOnly,
    /// Allocations to an out-of-period PO do not count as covering the
    /// bill, so the payment gate blocks until a person overrides.
    RequireFullCover,
}

impl OverPeriod {
    pub fn as_str(self) -> &'static str {
        match self {
            OverPeriod::FlagOnly => "flag_only",
            OverPeriod::RequireFullCover => "require_full_cover",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "flag_only" => OverPeriod::FlagOnly,
            "require_full_cover" => OverPeriod::RequireFullCover,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Policies {
    pub over_period: OverPeriod,
}

impl Default for Policies {
    /// The safe defaults: flag; do not silently match.
    fn default() -> Self {
        Policies {
            over_period: OverPeriod::FlagOnly,
        }
    }
}

/// Whether an invoice date falls outside a PO's service period. A PO with
/// no period, or a period with only one end, only checks the ends it has.
pub fn outside_period(
    period_start: Option<chrono::NaiveDate>,
    period_end: Option<chrono::NaiveDate>,
    invoice_date: chrono::NaiveDate,
) -> bool {
    period_start.is_some_and(|s| invoice_date < s) || period_end.is_some_and(|e| invoice_date > e)
}

/// Which shortfall exception a partly covered bill carries: `over_available`
/// when the vendor's other active POs cannot cover the rest either (the
/// decided cumulative caps are the limit), `uncovered` otherwise.
pub fn shortfall_kind(covered: Cents, bill_cents: Cents, others_available: Cents) -> &'static str {
    if covered + others_available < bill_cents {
        "over_available"
    } else {
        "uncovered"
    }
}

/// A PO's billing position, derived from the allocation ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Amounts {
    pub authorized_cents: Cents,
    pub reserved_cents: Cents,
    pub consumed_cents: Cents,
    pub allowance_cents: Cents,
}

impl Amounts {
    /// Reserved + consumed may exceed authorized by at most the allowance.
    pub fn ceiling_cents(&self) -> Cents {
        self.authorized_cents + self.allowance_cents
    }
    pub fn committed_cents(&self) -> Cents {
        self.reserved_cents + self.consumed_cents
    }
    /// How much more can be reserved right now (never negative).
    pub fn available_cents(&self) -> Cents {
        (self.ceiling_cents() - self.committed_cents()).max(0)
    }
    /// Derived billing progress; never stored as lifecycle.
    pub fn progress(&self) -> &'static str {
        if self.committed_cents() == 0 {
            "unbilled"
        } else if self.committed_cents() < self.authorized_cents {
            "partially_billed"
        } else {
            "fully_billed"
        }
    }
    /// Would reserving `amount` stay within available + allowance?
    pub fn can_reserve(&self, amount: Cents) -> bool {
        amount > 0 && self.committed_cents() + amount <= self.ceiling_cents()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_and_shortfalls() {
        use chrono::NaiveDate;
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert!(!outside_period(None, None, d("2027-06-01")));
        assert!(!outside_period(
            Some(d("2027-01-01")),
            Some(d("2027-12-31")),
            d("2027-06-01")
        ));
        assert!(outside_period(
            Some(d("2027-01-01")),
            Some(d("2027-12-31")),
            d("2028-01-01")
        ));
        assert!(outside_period(None, Some(d("2027-01-31")), d("2027-02-01")));
        assert!(outside_period(Some(d("2027-02-01")), None, d("2027-01-15")));
        assert_eq!(Policies::default().over_period, OverPeriod::FlagOnly);
        assert_eq!(shortfall_kind(10_000, 50_000, 30_000), "over_available");
        assert_eq!(shortfall_kind(10_000, 50_000, 40_000), "uncovered");
        assert_eq!(
            OverPeriod::parse("require_full_cover"),
            Some(OverPeriod::RequireFullCover)
        );
        assert_eq!(OverPeriod::parse("script"), None);
    }

    #[test]
    fn allowance_is_the_smaller_cap_and_absent_is_zero() {
        // $1,000 PO, $25 and 5% (500bp) caps -> $25 governs, ceiling $1,025.
        assert_eq!(allowance_cents(100_000, Some(2_500), Some(500)), 2_500);
        assert_eq!(allowance_cents(100_000, None, Some(500)), 5_000);
        assert_eq!(allowance_cents(100_000, Some(2_500), None), 2_500);
        assert_eq!(allowance_cents(100_000, None, None), 0);
        assert_eq!(allowance_cents(100_000, Some(0), Some(500)), 0);
    }

    #[test]
    fn partial_bills_need_not_equal_the_po_total() {
        let a = Amounts {
            authorized_cents: 100_000,
            reserved_cents: 0,
            consumed_cents: 40_000,
            allowance_cents: 0,
        };
        assert_eq!(a.progress(), "partially_billed");
        assert!(a.can_reserve(60_000));
        assert!(!a.can_reserve(60_001));
        let full = Amounts {
            consumed_cents: 100_000,
            ..a
        };
        assert_eq!(full.progress(), "fully_billed");
    }

    #[test]
    fn overage_is_cumulative_across_invoices() {
        // Two $600 invoices, $1,000 PO, $100 allowance: the second fails even
        // though each is under the PO total on its own.
        let mut a = Amounts {
            authorized_cents: 100_000,
            reserved_cents: 0,
            consumed_cents: 0,
            allowance_cents: 10_000,
        };
        assert!(a.can_reserve(60_000));
        a.consumed_cents = 60_000;
        assert!(!a.can_reserve(60_000));
        assert!(a.can_reserve(50_000));
    }
}
