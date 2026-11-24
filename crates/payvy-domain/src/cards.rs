//! Cards on file: a vendor keeps one of the company's cards and charges it.
//! The vendor pulls the money, so Payvy's part is to say which bill each
//! charge paid — only when the answer is plain — and to show the rest to a
//! person. Pure: the server supplies the charges, the bills and the dates.

use chrono::NaiveDate;

use crate::Cents;

/// Where a card charge stands, from the bank's word for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeStatus {
    /// Authorized, not settled: it can still change or drop.
    Pending,
    /// Settled: the money went.
    Posted,
    /// Cancelled, failed, blocked or reversed: the money didn't go, or came back.
    Reversed,
}

impl ChargeStatus {
    /// Mercury's transaction status. Anything new counts as not paid, so a
    /// person looks rather than Payvy assuming the money went.
    pub fn from_mercury(s: &str) -> Self {
        match s {
            "pending" => ChargeStatus::Pending,
            "sent" => ChargeStatus::Posted,
            _ => ChargeStatus::Reversed,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ChargeStatus::Pending => "pending",
            ChargeStatus::Posted => "posted",
            ChargeStatus::Reversed => "reversed",
        }
    }
}

/// How far a bill's invoice date may be from the charge: vendors charge on
/// the day, a little before the invoice reaches anyone, or on terms.
pub const DAYS_BEFORE_INVOICE: i64 = 14;
pub const DAYS_AFTER_INVOICE: i64 = 60;

/// A posted charge on the vendor's card that pays no bill yet.
#[derive(Debug, Clone)]
pub struct OpenCharge {
    pub amount_cents: Cents,
    pub charged_on: NaiveDate,
}

/// An open bill of the same vendor that no charge pays yet.
#[derive(Debug, Clone)]
pub struct OpenBill {
    pub amount_cents: Cents,
    pub invoice_date: NaiveDate,
}

fn fits(c: &OpenCharge, b: &OpenBill) -> bool {
    let days = (c.charged_on - b.invoice_date).num_days();
    c.amount_cents == b.amount_cents && (-DAYS_BEFORE_INVOICE..=DAYS_AFTER_INVOICE).contains(&days)
}

/// The charge-to-bill pairs that are plain, as (charge index, bill index).
/// A pair is plain only when that bill is the charge's one fit and that
/// charge is the bill's one fit: same amount to the cent, invoice dated
/// near the charge. Two $750 bills, or two $750 charges, are left for a
/// person — never the nearest date, never a partial amount.
pub fn plain_matches(charges: &[OpenCharge], bills: &[OpenBill]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (ci, c) in charges.iter().enumerate() {
        let mut fit = bills.iter().enumerate().filter(|(_, b)| fits(c, b));
        let (Some((bi, b)), None) = (fit.next(), fit.next()) else {
            continue;
        };
        if charges.iter().filter(|other| fits(other, b)).count() == 1 {
            out.push((ci, bi));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    fn charge(cents: Cents, on: &str) -> OpenCharge {
        OpenCharge {
            amount_cents: cents,
            charged_on: d(on),
        }
    }
    fn bill(cents: Cents, on: &str) -> OpenBill {
        OpenBill {
            amount_cents: cents,
            invoice_date: d(on),
        }
    }

    #[test]
    fn mercury_statuses_never_assume_the_money_went() {
        assert_eq!(ChargeStatus::from_mercury("pending"), ChargeStatus::Pending);
        assert_eq!(ChargeStatus::from_mercury("sent"), ChargeStatus::Posted);
        for s in ["cancelled", "failed", "reversed", "blocked", "new-thing"] {
            assert_eq!(ChargeStatus::from_mercury(s), ChargeStatus::Reversed, "{s}");
        }
    }

    #[test]
    fn only_a_one_to_one_fit_matches() {
        // One $710 charge, one $710 bill two weeks earlier, one $750 bill.
        let charges = [charge(71_000, "2026-11-01")];
        let bills = [bill(75_000, "2026-11-20"), bill(71_000, "2026-10-15")];
        assert_eq!(plain_matches(&charges, &bills), vec![(0, 1)]);

        // A cent off is no match.
        assert!(plain_matches(&[charge(71_001, "2026-11-01")], &bills).is_empty());

        // Two bills for the same amount: a person chooses.
        let twins = [bill(75_000, "2026-11-19"), bill(75_000, "2026-11-20")];
        assert!(plain_matches(&[charge(75_000, "2026-11-21")], &twins).is_empty());

        // Two charges for one bill: a person chooses which paid it.
        let both = [charge(75_000, "2026-11-21"), charge(75_000, "2026-11-22")];
        assert!(plain_matches(&both, &[bill(75_000, "2026-11-20")]).is_empty());

        // Two charges, two bills, amounts tell them apart.
        let charges = [charge(75_000, "2026-11-21"), charge(71_000, "2026-11-01")];
        let bills = [bill(71_000, "2026-10-15"), bill(75_000, "2026-11-20")];
        assert_eq!(plain_matches(&charges, &bills), vec![(0, 1), (1, 0)]);
    }

    #[test]
    fn the_invoice_date_must_be_near_the_charge() {
        let b = [bill(75_000, "2026-11-20")];
        // Charged up to two weeks before the invoice date, or up to sixty days after.
        assert_eq!(plain_matches(&[charge(75_000, "2026-11-06")], &b).len(), 1);
        assert!(plain_matches(&[charge(75_000, "2026-11-05")], &b).is_empty());
        assert_eq!(plain_matches(&[charge(75_000, "2027-01-19")], &b).len(), 1);
        assert!(plain_matches(&[charge(75_000, "2027-01-20")], &b).is_empty());
    }
}
