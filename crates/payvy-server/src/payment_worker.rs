//! The payment checker: asks the bank about every payment still going, and
//! about sent ones for two weeks (an ACH can come back as returned), through
//! the same `payment.refresh` command anyone can run, as the agent `bank`.
//! It also checks open Mercury invitations every few minutes through
//! `vendor.refresh_payee_invite`, so a completed one turns into a proposed
//! payee without anyone pressing "Check invitation". A person still accepts it.
//! And it pays bills whose workflow pays automatically (`bill.auto_pay`, as
//! the agent `workflow`) once they reach that step. Every few minutes it
//! reads the charges on cards vendors keep on file (`card.sync`), which marks
//! bills paid when a charge plainly pays them.

use std::time::Duration;

use payvy_store as store;
use serde_json::json;

use crate::commands::{self, Mode};
use crate::{Actor, AppState};

pub const ACTOR: &str = "bank";
const EVERY: Duration = Duration::from_secs(60);
const PER_TICK: i64 = 50;
/// Vendors take days to answer an invitation; minutes is plenty.
const INVITES_EVERY: chrono::Duration = chrono::Duration::minutes(5);
/// Card charges post within a day; a few minutes is prompt enough.
const CARDS_EVERY: Duration = Duration::from_secs(300);

pub async fn run(state: AppState) {
    let mut cards_at: Option<std::time::Instant> = None;
    loop {
        if cards_at.is_none_or(|t| t.elapsed() >= CARDS_EVERY) {
            cards_at = Some(std::time::Instant::now());
            if let Err(e) = sync_cards(&state).await {
                tracing::warn!("card checker: {e:#}");
            }
        }
        if let Err(e) = tick(&state).await {
            tracing::warn!("payment checker: {e:#}");
        }
        if let Err(e) = auto_pay(&state).await {
            tracing::warn!("auto-pay: {e:#}");
        }
        if let Err(e) = check_invites(&state).await {
            tracing::warn!("invitation checker: {e:#}");
        }
        tokio::time::sleep(EVERY).await;
    }
}

/// One pass over every organization. Returns how many payments changed.
pub async fn tick(state: &AppState) -> anyhow::Result<usize> {
    let mut changed = 0;
    for org in state.store.org_ids().await? {
        let mut tx = state.store.begin(org).await?;
        let ids = store::payments_to_check(
            &mut tx,
            (state.clock)(),
            chrono::Duration::minutes(2),
            chrono::Duration::days(14),
            PER_TICK,
        )
        .await?;
        tx.rollback().await?;
        for id in ids {
            let mode = Mode::Commit {
                confirm_token: None,
            };
            match commands::run(
                state,
                &Actor::agent(org, ACTOR),
                "payment.refresh",
                json!({ "payment_id": id }),
                mode,
            )
            .await
            {
                Ok(out) if out["result"]["changed"] == true => changed += 1,
                Ok(_) => {}
                Err(e) => tracing::warn!(payment = %id, "payment checker: {e}"),
            }
        }
    }
    Ok(changed)
}

/// One pass over open invitations not checked in the last few minutes.
/// Returns how many turned into payee proposals.
pub async fn check_invites(state: &AppState) -> anyhow::Result<usize> {
    let mut proposed = 0;
    for org in state.store.org_ids().await? {
        let mut tx = state.store.begin(org).await?;
        let ids =
            store::invites_to_check(&mut tx, (state.clock)(), INVITES_EVERY, PER_TICK).await?;
        tx.rollback().await?;
        for id in ids {
            let mode = Mode::Commit {
                confirm_token: None,
            };
            match commands::run(
                state,
                &Actor::agent(org, ACTOR),
                "vendor.refresh_payee_invite",
                json!({ "invite_id": id }),
                mode,
            )
            .await
            {
                Ok(out) if !out["result"]["invite"]["proposal_id"].is_null() => proposed += 1,
                Ok(_) => {}
                // "Not responded yet" and the like: try again next time.
                Err(commands::CommandError::Conflict(_)) => {}
                Err(e) => tracing::warn!(invite = %id, "invitation checker: {e}"),
            }
            let mut tx = state.store.begin(org).await?;
            store::mark_invite_checked(&mut tx, id, (state.clock)()).await?;
            tx.commit().await?;
        }
    }
    Ok(proposed)
}

/// Pays every bill that has reached an automatic pay step. Bills that can't
/// be paid this way yet (no accepted payee, no Mercury) wait quietly.
/// Returns how many payments were attempted.
pub async fn auto_pay(state: &AppState) -> anyhow::Result<usize> {
    let mut attempted = 0;
    for org in state.store.org_ids().await? {
        // Bills wait at their pay step while the plan excludes payments.
        if !crate::entitlements::company_allows(state, org, crate::entitlements::Need::Payments)
            .await?
        {
            continue;
        }
        let actor = Actor::agent(org, "workflow");
        let ready = commands::run(
            state,
            &actor,
            "bill.list",
            json!({ "status": "ready_to_pay", "limit": PER_TICK }),
            Mode::Commit {
                confirm_token: None,
            },
        )
        .await?;
        let ids: Vec<String> = ready["result"]["bills"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["bill"]["id"].as_str().map(str::to_owned))
            .collect();
        for id in ids {
            let mode = Mode::Commit {
                confirm_token: None,
            };
            match commands::run(
                state,
                &actor,
                "bill.auto_pay",
                json!({ "bill_id": id }),
                mode,
            )
            .await
            {
                Ok(_) => attempted += 1,
                Err(commands::CommandError::Conflict(_)) => {}
                Err(e) => tracing::warn!(bill = %id, "auto-pay: {e}"),
            }
        }
    }
    Ok(attempted)
}

/// Reads card charges for every company with a card on file. Returns how
/// many bills it marked paid.
pub async fn sync_cards(state: &AppState) -> anyhow::Result<usize> {
    let mut paid = 0;
    for org in state.store.org_ids().await? {
        let mut tx = state.store.begin(org).await?;
        let any = match store::live_paying_connection(&mut tx).await? {
            Some(c) if c.provider == "mercury" => !store::cards::vendor_cards_at(&mut tx, c.id)
                .await?
                .is_empty(),
            _ => false,
        };
        tx.rollback().await?;
        if !any {
            continue;
        }
        match commands::run(
            state,
            &Actor::agent(org, ACTOR),
            "card.sync",
            json!({}),
            Mode::Commit {
                confirm_token: None,
            },
        )
        .await
        {
            Ok(out) => paid += out["result"]["paid"].as_u64().unwrap_or(0) as usize,
            Err(e) => tracing::warn!(%org, "card checker: {e}"),
        }
    }
    Ok(paid)
}
