//! Plan entitlements for hosted Payvy (docs/hosted.md).
//!
//! A hosted company's plan says two things: whether Payvy's built-in reader
//! may read its documents (`reader`), and whether it may send new payments
//! through its bank (`payments`). Only the operator sets them
//! (`entitlement.set`); a company with nothing stored is provisional, with
//! both off. Self-hosted installs have no plans and never check.
//!
//! What a lapse never stops: signing in, reading and exporting, status
//! polling and bank webhooks (`payment.refresh`), resending an attempt that
//! is already `submitting` with its own key, and `bill.mark_paid` for a
//! payment made elsewhere.

use payvy_store::{self as store, Entitlement};
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// A paid call to the model provider (`document.extract`, and the
    /// workers that run it).
    Reader,
    /// A new payment through the bank (`bill.release` with a paying bank,
    /// `bill.auto_pay`, `payments::submit`).
    Payments,
}

impl Need {
    pub fn refusal(self) -> &'static str {
        match self {
            Need::Reader => {
                "this plan reads invoices with your own agent: upload the document, read it with your agent, and create the bill with bill.create"
            }
            Need::Payments => {
                "this company's plan doesn't include payments right now: an Admin can check Settings > Billing. You can still pay outside Payvy and record it with bill.mark_paid"
            }
        }
    }
}

/// What a stored entitlement allows. None (no row) is provisional: nothing.
pub fn allows(stored: Option<&Entitlement>, need: Need) -> bool {
    stored.is_some_and(|e| match need {
        Need::Reader => e.reader,
        Need::Payments => e.payments,
    })
}

/// For the workers, which skip a company rather than fail each item: does
/// the plan include `need`? Always true when self-hosted.
pub async fn company_allows(state: &AppState, org: Uuid, need: Need) -> anyhow::Result<bool> {
    if !state.hosted {
        return Ok(true);
    }
    let mut tx = state.store.begin(org).await?;
    let stored = store::entitlement(&mut tx, org).await?;
    tx.rollback().await?;
    Ok(allows(stored.as_ref(), need))
}

/// What a company sees about its own plan (`org.get`).
pub fn view(hosted: bool, stored: Option<&Entitlement>) -> serde_json::Value {
    if !hosted {
        return serde_json::json!({ "hosted": false, "reader": true, "payments": true });
    }
    serde_json::json!({
        "hosted": true,
        "provisional": stored.is_none(),
        "reader": allows(stored, Need::Reader),
        "payments": allows(stored, Need::Payments),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(reader: bool, payments: bool) -> Entitlement {
        Entitlement {
            org_id: Uuid::nil(),
            reader,
            payments,
            version: 1,
            updated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            updated_by: "operator".into(),
        }
    }

    #[test]
    fn nothing_stored_is_provisional() {
        assert!(!allows(None, Need::Reader));
        assert!(!allows(None, Need::Payments));
    }

    #[test]
    fn each_flag_gates_only_its_own_need() {
        let byoa = stored(false, true);
        assert!(!allows(Some(&byoa), Need::Reader));
        assert!(allows(Some(&byoa), Need::Payments));
        let lapsed = stored(false, false);
        assert!(!allows(Some(&lapsed), Need::Payments));
    }

    #[test]
    fn self_hosted_view_has_everything_on() {
        let v = view(false, None);
        assert_eq!(v["reader"], true);
        assert_eq!(v["payments"], true);
        assert_eq!(view(true, None)["provisional"], true);
    }
}
