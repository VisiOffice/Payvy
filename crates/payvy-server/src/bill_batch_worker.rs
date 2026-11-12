//! The batch worker: after `document.create_bills` splits a file of several
//! invoices, each part is read here (through `document.extract`, as the
//! background reader reads, under its name) and made a bill (through
//! `bill.create`, as the person who asked for the batch, so their role still
//! applies and the bill's record names them). A part that reads cleanly
//! becomes a bill; a copy of a bill already here stacks on it; anything
//! else is left in To review for a person, with the reason.
//!
//! It runs whenever a model is configured, even with the background reader
//! off: the person asked for these parts to be read. A new batch wakes it
//! at once (`AppState::bill_batches`); otherwise it looks every 15 seconds.

use std::time::Duration;

use payvy_store as store;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::commands::{self, CommandError, Mode};
use crate::{Actor, AppState};

const EVERY: Duration = Duration::from_secs(15);
/// How long a part is held by one worker while it is read and billed. Parts
/// are claimed one at a time, so the lease covers one read (the model is
/// given at most five minutes), not a whole pass: a pass of twenty parts at
/// half a minute each would outlast it, and another worker would take parts
/// this one still meant to read.
const LEASE_MINUTES: i64 = 10;
/// Parts looked at in one pass, before the worker yields to the next org.
const PER_TICK: i64 = 20;
/// Failed reads before a part is left for a person.
const MAX_FAILURES: i32 = 3;

/// Runs until the process stops.
pub async fn run(state: AppState) {
    loop {
        match tick(&state).await {
            // More may be waiting: go again straight away.
            Ok(n) if n as i64 >= PER_TICK => continue,
            Ok(_) => {}
            Err(e) => tracing::warn!("bill batches: {e:#}"),
        }
        tokio::select! {
            _ = state.bill_batches.notified() => {}
            _ = tokio::time::sleep(EVERY) => {}
        }
    }
}

/// One pass over every organization: each part still being read is read if
/// it hasn't been, then billed or left for a person. Returns how many parts
/// were looked at.
pub async fn tick(state: &AppState) -> anyhow::Result<usize> {
    let mut done = 0;
    for org in state.store.org_ids().await? {
        for _ in 0..PER_TICK {
            let mut tx = state.store.begin(org).await?;
            let claimed = store::claim_bill_batch_parts(
                &mut tx,
                (state.clock)(),
                chrono::Duration::minutes(LEASE_MINUTES),
                1,
            )
            .await?;
            tx.commit().await?;
            let Some((part, requested_by)) = claimed.into_iter().next() else {
                break;
            };
            done += 1;
            let outcome = step(state, org, &part, &requested_by).await;
            let mut tx = state.store.begin(org).await?;
            let now = (state.clock)();
            match outcome {
                Outcome::Done {
                    state: s,
                    bill,
                    note,
                } => {
                    store::finish_bill_batch_part(&mut tx, part.id, s, bill, note.as_deref(), now)
                        .await?;
                }
                Outcome::Later { failed, why } => {
                    let tries =
                        store::release_bill_batch_part(&mut tx, part.id, failed, now).await?;
                    if tries >= MAX_FAILURES {
                        let note = format!("Needs you: it couldn't be read ({why})");
                        store::finish_bill_batch_part(
                            &mut tx,
                            part.id,
                            "needs_you",
                            None,
                            Some(&note),
                            now,
                        )
                        .await?;
                    }
                }
            }
            tx.commit().await?;
        }
    }
    Ok(done)
}

enum Outcome {
    Done {
        state: &'static str,
        bill: Option<Uuid>,
        note: Option<String>,
    },
    /// Not decided this pass: being read by someone else, or the read failed.
    Later { failed: bool, why: String },
}

fn needs(note: impl Into<String>) -> Outcome {
    Outcome::Done {
        state: "needs_you",
        bill: None,
        note: Some(note.into()),
    }
}

async fn step(
    state: &AppState,
    org: Uuid,
    part: &store::BillBatchPart,
    requested_by: &str,
) -> Outcome {
    let reader = Actor::agent(org, crate::extract_worker::ACTOR);
    let commit = || Mode::Commit {
        confirm_token: None,
    };
    let id = json!({ "document_id": part.document_id });
    let get = match commands::run(state, &reader, "document.get", id.clone(), commit()).await {
        Ok(out) => out,
        Err(e) => {
            return Outcome::Later {
                failed: true,
                why: e.to_string(),
            };
        }
    };
    let doc = &get["result"]["document"];
    if doc["status"] != "inbox" {
        // A person filed, set aside or put back this part before it was billed.
        let bill = doc["bill_id"].as_str().and_then(|b| b.parse().ok());
        let note = match doc["status"].as_str() {
            Some("filed") => "Filed by a person meanwhile",
            Some("trashed") => "Set aside as not a bill meanwhile",
            _ => "Put back in the file meanwhile",
        };
        return Outcome::Done {
            state: "withdrawn",
            bill,
            note: Some(note.into()),
        };
    }

    let mut reading = get["result"]["extractions"][0].clone();
    if reading.is_null() {
        if state.extractor.is_none() {
            return needs("Needs you: no model is set up to read it");
        }
        match commands::run(state, &reader, "document.extract", id, commit()).await {
            Ok(out) => reading = out["result"].clone(),
            // Being read right now (the background reader, or a person).
            Err(CommandError::Conflict(e)) => {
                return Outcome::Later {
                    failed: false,
                    why: e,
                };
            }
            Err(e) => {
                return Outcome::Later {
                    failed: true,
                    why: e.to_string(),
                };
            }
        }
    }
    decide(state, org, &reading, requested_by).await
}

/// Billed if the reading is clean; else what a person must look at.
async fn decide(state: &AppState, org: Uuid, reading: &Value, requested_by: &str) -> Outcome {
    if reading["extraction"]["status"] == "failed" {
        let why = reading["extraction"]["error"]
            .as_str()
            .unwrap_or("the model failed");
        return needs(format!("Needs you: it couldn't be read ({why})"));
    }
    // A copy of an invoice already here isn't a reason to stop: bill.create
    // stacks it on that bill.
    let blocking: Vec<&str> = reading["failed_checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|c| *c != "not_a_duplicate")
        .collect();
    if !blocking.is_empty() {
        let details: Vec<String> = reading["extraction"]["checks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["check"].as_str().is_some_and(|k| blocking.contains(&k)))
            .filter_map(|c| c["detail"].as_str().map(str::to_owned))
            .collect();
        let why = if details.is_empty() {
            blocking.join(", ")
        } else {
            details.join("; ")
        };
        return needs(format!("Needs you: {why}"));
    }
    if reading["review_required"] == true {
        return needs("Needs you: its line items need a review first");
    }
    let input = &reading["bill_input"];
    if input.is_null() {
        return needs("Needs you: not enough was read to make a bill");
    }
    let person = Actor::person(org, requested_by);
    let mode = Mode::Commit {
        confirm_token: None,
    };
    match commands::run(state, &person, "bill.create", input.clone(), mode).await {
        Ok(out) => {
            let r = &out["result"];
            let bill = r["bill"]["id"].as_str().and_then(|b| b.parse().ok());
            match r["arrival"]["kind"].as_str() {
                Some(kind) => Outcome::Done {
                    state: "stacked",
                    bill,
                    note: Some(if kind == "revision" {
                        format!(
                            "Stacked on an existing bill as a revision (received {} times); it can't be paid until a person accepts or rejects it",
                            r["arrival"]["received_count"]
                        )
                    } else {
                        format!(
                            "Stacked on an existing bill as a resend (received {} times)",
                            r["arrival"]["received_count"]
                        )
                    }),
                },
                None => Outcome::Done {
                    state: "created",
                    bill,
                    note: None,
                },
            }
        }
        Err(e) => needs(format!("Needs you: {e}")),
    }
}
