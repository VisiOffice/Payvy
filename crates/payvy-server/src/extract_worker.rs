//! The background reader: inbox documents nobody has read yet are read by
//! the agent `extractor`, through the same `document.extract` command an
//! agent or person would call. A failed read is recorded on the document and
//! retried later, up to three times; after that it waits for a person.
//! A file the reader finds several invoices in is split, one document per
//! invoice (`document.extract`), and each part is read straight after.

use std::time::Duration;

use payvy_store as store;
use serde_json::json;

use crate::commands::{self, CommandError, Mode};
use crate::{Actor, AppState};

/// The actor recorded on automatic reads.
pub const ACTOR: &str = "extractor";
const EVERY: Duration = Duration::from_secs(30);
const MAX_FAILURES: i64 = 3;
const PER_TICK: i64 = 10;

/// Runs until the process stops.
pub async fn run(state: AppState) {
    loop {
        if let Err(e) = tick(&state).await {
            tracing::warn!("document reader: {e:#}");
        }
        tokio::time::sleep(EVERY).await;
    }
}

/// One pass over every organization. Returns how many documents were read
/// (or tried and recorded as failed).
pub async fn tick(state: &AppState) -> anyhow::Result<usize> {
    let mut done = 0;
    for org in state.store.org_ids().await? {
        // A plan without the built-in reader: its own agent reads. With it,
        // only what arrived since the reader was switched on is read
        // automatically, so an upgrade doesn't read a backlog unasked.
        let Some(since) = crate::entitlements::auto_read_since(state, org).await? else {
            continue;
        };
        let mut tx = state.store.begin(org).await?;
        let ids = store::documents_to_extract(
            &mut tx,
            (state.clock)(),
            chrono::Duration::minutes(15),
            MAX_FAILURES,
            PER_TICK,
            since,
        )
        .await?;
        tx.rollback().await?;
        let mut queue: std::collections::VecDeque<uuid::Uuid> = ids.into();
        while let Some(id) = queue.pop_front() {
            let actor = Actor::agent(org, ACTOR);
            let mode = Mode::Commit {
                confirm_token: None,
            };
            match commands::run(
                state,
                &actor,
                "document.extract",
                json!({ "document_id": id }),
                mode,
            )
            .await
            {
                Ok(out) => {
                    done += 1;
                    // A file of several invoices was split: read each part
                    // now. Parts are never split again, so this ends.
                    let parts = out["result"]["split"]["to_read"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str()?.parse::<uuid::Uuid>().ok());
                    queue.extend(parts);
                }
                // Read meanwhile by someone else, or filed or trashed.
                Err(CommandError::Conflict(_)) => {}
                Err(e) => tracing::warn!(document = %id, "document reader: {e}"),
            }
        }
    }
    Ok(done)
}
