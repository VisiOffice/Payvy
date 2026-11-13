//! The ledger worker: queued posting jobs (queued by the approval and
//! payment hooks, in the same transaction as the fact that caused them)
//! are posted here, each in its own transaction, so a slow or failing
//! ledger never stalls or rolls back an approval or a bank fact. A
//! failure is recorded on the job and on the bill's ledger link, and
//! retried with backoff; a person can always post by hand meanwhile.

use std::time::Duration;

use payvy_store as store;

use crate::AppState;
use crate::commands;

const EVERY: Duration = Duration::from_secs(15);
const PER_TICK: i64 = 20;

/// Whether a failure says nothing about the job itself: the ledger
/// couldn't be reached or didn't answer in time, or Payvy's own store
/// hiccuped. Those retry for as long as they take to clear; a refusal
/// from the ledger (a closed period, an unmapped account) is permanent
/// and stops at the attempt cap.
fn transient(error: &commands::CommandError) -> bool {
    use commands::CommandError;
    match error {
        CommandError::Ledger { code, .. } => matches!(*code, "timeout" | "concurrency"),
        CommandError::Internal(_) => true,
        _ => false,
    }
}

/// Runs until the process stops.
pub async fn run(state: AppState) {
    loop {
        match tick(&state).await {
            Ok(n) if n as i64 >= PER_TICK => continue,
            Ok(_) => {}
            Err(e) => tracing::warn!("ledger jobs: {e:#}"),
        }
        tokio::time::sleep(EVERY).await;
    }
}

/// One pass over every organization with due jobs. Returns how many jobs
/// were claimed.
pub async fn tick(state: &AppState) -> anyhow::Result<usize> {
    let mut claimed_total = 0;
    for org in state.store.org_ids().await? {
        let now = (state.clock)();
        let claimed = {
            let mut tx = state.store.begin(org).await?;
            let jobs = store::claim_ledger_jobs(&mut tx, now, PER_TICK).await?;
            tx.commit().await?;
            jobs
        };
        claimed_total += claimed.len();
        for job in claimed {
            let result = commands::execute_job(state, org, &job).await;
            let mut tx = state.store.begin(org).await?;
            match result {
                Ok(commands::JobOutcome::Done) => {
                    store::finish_ledger_job(&mut tx, job.id, (state.clock)()).await?
                }
                Ok(commands::JobOutcome::Defer) => {
                    let when = (state.clock)() + chrono::Duration::minutes(15);
                    store::defer_ledger_job(&mut tx, job.id, when, (state.clock)()).await?
                }
                Err(e) => {
                    let message = e.to_string();
                    let message: String = message.chars().take(300).collect();
                    store::fail_ledger_job(
                        &mut tx,
                        job.id,
                        &message,
                        (state.clock)(),
                        transient(&e),
                    )
                    .await?;
                }
            }
            tx.commit().await?;
        }
    }
    Ok(claimed_total)
}
