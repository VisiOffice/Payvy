//! Renews and checks existing Ramp grants through the command registry.
//! Runs separately from payment polling so Ramp outages cannot delay Mercury.
use crate::{
    Actor, AppState,
    commands::{self, CommandError, Mode},
};
use payvy_store::ramp;
use serde_json::json;

pub async fn run(state: AppState) {
    loop {
        if let Err(error) = tick(&state).await {
            tracing::warn!("Ramp connection checker: {error:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}
/// At most fifty due connections per pass; persisted cooldowns are also checked
/// under the connection lock inside the command, across all server processes.
pub async fn tick(state: &AppState) -> anyhow::Result<usize> {
    if state.bank.as_ref().is_none_or(|b| b.ramp.is_none()) {
        return Ok(0);
    }
    let mut checked = 0;
    let mut attempted = 0;
    for org in state.store.org_ids().await? {
        let mut tx = state.store.begin(org).await?;
        let ids = ramp::due(&mut tx, (state.clock)(), 50 - attempted).await?;
        tx.rollback().await?;
        for id in ids {
            attempted += 1;
            match commands::run(
                state,
                &Actor::agent(org, "bank"),
                "bank.maintain",
                json!({"connection_id":id}),
                Mode::Commit {
                    confirm_token: None,
                },
            )
            .await
            {
                Ok(_) => checked += 1,
                // Another worker, a preview, or a person may have changed it.
                Err(CommandError::Conflict(_) | CommandError::NotFound(_)) => {}
                Err(error) => tracing::warn!(connection=%id,"Ramp connection checker: {error}"),
            }
        }
        if attempted >= 50 {
            break;
        }
    }
    Ok(checked)
}
