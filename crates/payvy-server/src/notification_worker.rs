//! Workflow notification reconciliation, independent of bank configuration.
//! Time-based holds, role changes and reminders are picked up without a new event.
use crate::{
    Actor, AppState,
    commands::{self, CommandError, Mode},
};
use payvy_store as store;
use serde_json::json;
use uuid::Uuid;

pub async fn run(state: AppState) {
    loop {
        if let Err(e) = tick(&state).await {
            tracing::warn!("notification worker: {e:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}
async fn reconcile(state: &AppState, org: Uuid) -> anyhow::Result<()> {
    let actor = Actor::agent(org, "notifications");
    let mut tx = state.store.begin(org).await?;
    let enabled = store::notification_settings(&mut tx, org).await?.enabled;
    tx.rollback().await?;
    if !enabled {
        return Ok(());
    }
    let mut after = None;
    loop {
        let mut tx = state.store.begin(org).await?;
        let ids = store::notification_bill_page(&mut tx, after).await?;
        tx.rollback().await?;
        if ids.is_empty() {
            break;
        }
        after = ids.last().copied();
        for id in ids {
            match commands::run(
                state,
                &actor,
                "notification.queue",
                json!({"bill_id":id}),
                Mode::Commit {
                    confirm_token: None,
                },
            )
            .await
            {
                Ok(_) | Err(CommandError::Conflict(_)) => {}
                Err(e) => tracing::warn!(bill=%id,"notification queue: {e}"),
            }
        }
    }
    Ok(())
}
/// Returns delivery attempts/cancellations settled through the command API.
pub async fn tick(state: &AppState) -> anyhow::Result<usize> {
    let mut changed = 0;
    for org in state.store.org_ids().await? {
        if let Err(e) = reconcile(state, org).await {
            tracing::warn!(%org,"notification reconciliation: {e:#}");
        }
        let mut tx = state.store.begin(org).await?;
        let ids = store::notifications_due(&mut tx, (state.clock)()).await?;
        tx.rollback().await?;
        // Bound external I/O concurrency. No lock is held during mail delivery.
        for batch in ids.chunks(5) {
            let mut tasks = tokio::task::JoinSet::new();
            for &id in batch {
                let state = state.clone();
                tasks.spawn(async move {
                    commands::run(
                        &state,
                        &Actor::agent(org, "notifications"),
                        "notification.deliver",
                        json!({"notification_id":id}),
                        Mode::Commit {
                            confirm_token: None,
                        },
                    )
                    .await
                });
            }
            while let Some(result) = tasks.join_next().await {
                match result {
                    Ok(Ok(_)) => changed += 1,
                    Ok(Err(CommandError::Conflict(_))) => {}
                    Ok(Err(e)) => tracing::warn!("notification delivery: {e}"),
                    Err(e) => tracing::warn!("notification task: {e}"),
                }
            }
        }
    }
    Ok(changed)
}
