//! The payment checker: asks the bank about every payment still going, and
//! about sent ones for two weeks (an ACH can come back as returned), through
//! the same `payment.refresh` command anyone can run, as the agent `bank`.

use std::time::Duration;

use payvy_store as store;
use serde_json::json;

use crate::commands::{self, Mode};
use crate::{Actor, AppState};

pub const ACTOR: &str = "bank";
const EVERY: Duration = Duration::from_secs(60);
const PER_TICK: i64 = 50;

pub async fn run(state: AppState) {
    loop {
        if let Err(e) = tick(&state).await {
            tracing::warn!("payment checker: {e:#}");
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
