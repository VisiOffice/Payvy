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
