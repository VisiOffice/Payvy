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

/// How quantity/rate findings are treated (quantity matching).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantityMatching {
    /// Findings show beside the bill; they never block (the default).
    FlagOnly,
    /// quantity_over and rate_mismatch block payment like other exceptions.
    Enforce,
}

impl QuantityMatching {
    pub fn as_str(self) -> &'static str {
        match self {
            QuantityMatching::FlagOnly => "flag_only",
            QuantityMatching::Enforce => "enforce",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "flag_only" => QuantityMatching::FlagOnly,
            "enforce" => QuantityMatching::Enforce,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Policies {
    pub over_period: OverPeriod,
    pub quantity_matching: QuantityMatching,
}

impl Default for Policies {
    /// The safe defaults: flag; do not silently match.
    fn default() -> Self {
        Policies {
            over_period: OverPeriod::FlagOnly,
            quantity_matching: QuantityMatching::FlagOnly,
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

/// A closed or canceled PO never takes an allocation again.
pub fn is_final(status: &str) -> bool {
    matches!(status, "closed" | "canceled")
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
    /// What a PO in `status` can still take. A closed or canceled PO is
    /// final and takes no allocation again, so nothing is available on it;
    /// reserved and consumed still report what stands. A draft or suspended
    /// PO keeps its figure: it is what activation or reactivation makes
    /// available, and allocation refuses either status meanwhile.
    pub fn available_cents_for(&self, status: &str) -> Cents {
        if is_final(status) {
            0
        } else {
            self.available_cents()
        }
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

/// One active PO of the bill's vendor, as automatic matching sees it.
#[derive(Debug, Clone)]
pub struct AutoCandidate<'a> {
    pub po_number: &'a str,
    pub vendor_reference: Option<&'a str>,
    pub service_address: Option<&'a str>,
    /// Authorized less reserved and consumed: the overage allowance is
    /// never used automatically.
    pub unbilled_cents: Cents,
    /// The bill's invoice date is inside the PO's service period (or the
    /// PO has none).
    pub in_period: bool,
}

/// Why a bill went against a PO without a person choosing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoRule {
    /// The invoice prints this PO's number (or the vendor's reference for it).
    PoNumber,
    /// The invoice's service address is this PO's, and no other open PO's.
    ServiceAddress,
}

impl AutoRule {
    pub fn as_str(self) -> &'static str {
        match self {
            AutoRule::PoNumber => "po_number",
            AutoRule::ServiceAddress => "service_address",
        }
    }
}

/// A PO number as compared: case, spaces, `#` and dashes don't count.
fn reference_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The street part of an address as compared: house number and street
/// name, lowercased, with the street type and punctuation dropped, so
/// "8932 Beverlywood Street, Los Angeles, CA" and "8932 BEVERLYWOOD ST."
/// agree. None unless it starts with a house number: a street name alone
/// is too loose to match on.
pub fn street_key(address: &str) -> Option<String> {
    const TYPES: [&str; 26] = [
        "street",
        "st",
        "avenue",
        "ave",
        "av",
        "road",
        "rd",
        "boulevard",
        "blvd",
        "drive",
        "dr",
        "lane",
        "ln",
        "way",
        "court",
        "ct",
        "place",
        "pl",
        "parkway",
        "pkwy",
        "circle",
        "cir",
        "terrace",
        "ter",
        "highway",
        "hwy",
    ];
    let first = address.split([',', '\n']).next()?;
    let words: Vec<String> = first
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .filter(|w| !TYPES.contains(&w.as_str()))
        .collect();
    let number = words.first()?;
    if !number.chars().next()?.is_ascii_digit() || words.len() < 2 {
        return None;
    }
    Some(words.join(" "))
}

/// The one PO a bill goes against without a person choosing, or None.
/// Only when exactly one answer is plain: the PO number printed on the
/// invoice names one candidate, or (with no PO number printed) exactly one
/// candidate in period has the invoice's service address. That PO must
/// cover the whole bill from its unbilled amount. A printed PO number that
/// names no candidate matches nothing rather than falling back to the
/// address. Anything else is left for a person.
pub fn auto_match(
    bill_cents: Cents,
    po_reference: Option<&str>,
    service_address: Option<&str>,
    candidates: &[AutoCandidate<'_>],
) -> Option<(usize, AutoRule)> {
    let reference = po_reference.map(reference_key).filter(|r| !r.is_empty());
    let (found, rule): (Vec<usize>, _) = if let Some(r) = reference {
        let hits = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                reference_key(c.po_number) == r
                    || c.vendor_reference.map(reference_key).as_deref() == Some(r.as_str())
            })
            .map(|(i, _)| i)
            .collect();
        (hits, AutoRule::PoNumber)
    } else {
        let key = street_key(service_address?)?;
        let hits = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.in_period && c.service_address.and_then(street_key) == Some(key.clone())
            })
            .map(|(i, _)| i)
            .collect();
        (hits, AutoRule::ServiceAddress)
    };
    let [one] = found[..] else {
        return None;
    };
    let c = &candidates[one];
    (c.in_period && bill_cents > 0 && c.unbilled_cents >= bill_cents).then_some((one, rule))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand<'a>(number: &'a str, address: Option<&'a str>, unbilled: Cents) -> AutoCandidate<'a> {
        AutoCandidate {
            po_number: number,
            vendor_reference: None,
            service_address: address,
            unbilled_cents: unbilled,
            in_period: true,
        }
    }

    #[test]
    fn street_keys() {
        assert_eq!(
            street_key("8932 Beverlywood Street, Los Angeles, CA, 90034").as_deref(),
            Some("8932 beverlywood")
        );
        assert_eq!(
            street_key("8932 BEVERLYWOOD ST.").as_deref(),
            Some("8932 beverlywood")
        );
        assert_eq!(street_key("Beverlywood Street"), None, "no house number");
        assert_eq!(street_key("8932"), None, "no street");
        assert_ne!(
            street_key("8932 Beverlywood St"),
            street_key("8934 Beverlywood St")
        );
    }

    #[test]
    fn auto_match_takes_only_a_plain_single_answer() {
        let bev = "8932 Beverlywood Street, Los Angeles, CA, 90034";
        let pos = [
            cand("Q-1", Some(bev), 75_000),
            cand("Q-2", Some("1500 Harwood St, LA"), 75_000),
        ];
        // The address names one PO that covers the bill.
        assert_eq!(
            auto_match(75_000, None, Some("8932 Beverlywood St"), &pos),
            Some((0, AutoRule::ServiceAddress))
        );
        // More than the PO has left: a person decides.
        assert_eq!(
            auto_match(75_001, None, Some("8932 Beverlywood St"), &pos),
            None
        );
        // No address, or one no PO has: nothing.
        assert_eq!(auto_match(75_000, None, None, &pos), None);
        assert_eq!(auto_match(75_000, None, Some("1 Elm St"), &pos), None);
        // The printed PO number wins, however it's punctuated.
        assert_eq!(
            auto_match(10_000, Some("q 2"), Some("8932 Beverlywood St"), &pos),
            Some((1, AutoRule::PoNumber))
        );
        // A printed number that names no PO never falls back to the address.
        assert_eq!(
            auto_match(10_000, Some("Q-9"), Some("8932 Beverlywood St"), &pos),
            None
        );

        // Two POs at the address: only the one whose period holds the
        // invoice date counts; two in period is a choice for a person.
        let mut periods = [
            cand("P-OCT", Some(bev), 50_000),
            cand("P-NOV", Some(bev), 50_000),
        ];
        periods[0].in_period = false;
        assert_eq!(
            auto_match(40_000, None, Some(bev), &periods),
            Some((1, AutoRule::ServiceAddress))
        );
        periods[0].in_period = true;
        assert_eq!(auto_match(40_000, None, Some(bev), &periods), None);

        // The vendor's own reference for a PO matches too, but not out of period.
        let mut referenced = [cand("Q-3", None, 20_000)];
        referenced[0].vendor_reference = Some("LB-778");
        assert_eq!(
            auto_match(20_000, Some("LB 778"), None, &referenced),
            Some((0, AutoRule::PoNumber))
        );
        referenced[0].in_period = false;
        assert_eq!(auto_match(20_000, Some("LB-778"), None, &referenced), None);
    }

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
    fn a_closed_or_canceled_po_has_nothing_available() {
        let a = Amounts {
            authorized_cents: 100_000,
            reserved_cents: 50_000,
            consumed_cents: 0,
            allowance_cents: 0,
        };
        assert_eq!(a.available_cents_for("active"), 50_000);
        assert_eq!(a.available_cents_for("suspended"), 50_000);
        assert_eq!(a.available_cents_for("draft"), 50_000);
        assert_eq!(a.available_cents_for("closed"), 0);
        assert_eq!(a.available_cents_for("canceled"), 0);
        // Reserved and consumed are untouched by the status.
        assert_eq!(a.committed_cents(), 50_000);
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
