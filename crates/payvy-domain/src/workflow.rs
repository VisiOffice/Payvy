//! Vendor workflows: the path a bill takes from arrival to payment.
//!
//! A workflow is an ordered list of steps that ends in exactly one `pay`
//! step. A bill pins the workflow version it arrived under, so editing a
//! workflow never changes a bill already in flight.
//!
//! Where a bill stands is never stored. [`evaluate`] derives it from facts
//! (approvals, signals, release) every time, the same way a balance is
//! computed from entries. That keeps the chart, the status and the audit
//! trail from disagreeing.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::Cents;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Stable within a workflow; approvals and the chart refer to it.
    pub id: String,
    #[serde(flatten)]
    pub kind: StepKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepKind {
    /// People sign off. An empty `approvers` list means any person in the
    /// organization. With `min_amount_cents`, bills below it skip the step.
    Approve {
        #[serde(default)]
        approvers: Vec<String>,
        #[serde(default)]
        require: Require,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_amount_cents: Option<Cents>,
    },
    /// Wait a number of days after the previous step finished.
    Hold { days: u32 },
    /// Wait until something outside Payvy happens, reported as a signal on
    /// the bill: e.g. `customer_funded` when the invoice that pays for this
    /// cost has been paid.
    WaitFor {
        signal: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// The last step. `auto` makes the bill ready to pay as soon as it gets
    /// here; `manual` waits for a person in `by` (empty: any person) to
    /// release it.
    Pay {
        #[serde(default)]
        release: Release,
        #[serde(default)]
        by: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Require {
    /// One of the approvers.
    #[default]
    Any,
    /// Every listed approver.
    All,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Release {
    #[default]
    Manual,
    Auto,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkflowError {
    #[error("a workflow needs at least a pay step")]
    Empty,
    #[error("step id {0:?} is used twice")]
    DuplicateId(String),
    #[error("step id {0:?} must be 1-40 characters of a-z, 0-9, _ or -")]
    BadId(String),
    #[error("the last step must be the only pay step")]
    PayNotLast,
    #[error("step {0}: require \"all\" needs a list of approvers")]
    AllWithoutApprovers(String),
    #[error("step {0}: an approver is listed twice")]
    DuplicateApprover(String),
    #[error("step {0}: min_amount_cents can't be negative")]
    NegativeAmount(String),
    #[error("step {0}: a hold is 1 to 365 days")]
    BadHold(String),
    #[error("step {0}: signal must be 1-60 characters of a-z, 0-9 or _")]
    BadSignal(String),
}

impl Workflow {
    /// The workflow used when a vendor has none and the organization has set
    /// no default: any person releases each bill for payment.
    pub fn builtin_default() -> Self {
        Workflow {
            steps: vec![Step {
                id: "pay".into(),
                kind: StepKind::Pay {
                    release: Release::Manual,
                    by: vec![],
                },
            }],
        }
    }

    pub fn validate(&self) -> Result<(), WorkflowError> {
        let Some(last) = self.steps.last() else {
            return Err(WorkflowError::Empty);
        };
        if !matches!(last.kind, StepKind::Pay { .. })
            || self
                .steps
                .iter()
                .filter(|s| matches!(s.kind, StepKind::Pay { .. }))
                .count()
                != 1
        {
            return Err(WorkflowError::PayNotLast);
        }
        let mut ids = HashSet::new();
        for step in &self.steps {
            let id = &step.id;
            if !(1..=40).contains(&id.len())
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
            {
                return Err(WorkflowError::BadId(id.clone()));
            }
            if !ids.insert(id.as_str()) {
                return Err(WorkflowError::DuplicateId(id.clone()));
            }
            match &step.kind {
                StepKind::Approve {
                    approvers,
                    require,
                    min_amount_cents,
                } => {
                    if *require == Require::All && approvers.is_empty() {
                        return Err(WorkflowError::AllWithoutApprovers(id.clone()));
                    }
                    if approvers.iter().collect::<HashSet<_>>().len() != approvers.len() {
                        return Err(WorkflowError::DuplicateApprover(id.clone()));
                    }
                    if min_amount_cents.is_some_and(|c| c < 0) {
                        return Err(WorkflowError::NegativeAmount(id.clone()));
                    }
                }
                StepKind::Hold { days } => {
                    if !(1..=365).contains(days) {
                        return Err(WorkflowError::BadHold(id.clone()));
                    }
                }
                StepKind::WaitFor { signal, .. } => {
                    if !valid_signal(signal) {
                        return Err(WorkflowError::BadSignal(id.clone()));
                    }
                }
                StepKind::Pay { by, .. } => {
                    if by.iter().collect::<HashSet<_>>().len() != by.len() {
                        return Err(WorkflowError::DuplicateApprover(id.clone()));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    /// A Mermaid flowchart of the workflow, for agents and docs. The web app
    /// draws its own chart from the same steps.
    pub fn mermaid(&self) -> String {
        let mut out = String::from("flowchart LR\n  received([Bill received])\n");
        let mut prev = "received".to_owned();
        for (i, step) in self.steps.iter().enumerate() {
            let node = format!("s{i}");
            let label = step.describe().replace('"', "'");
            let shape = match step.kind {
                StepKind::Pay { .. } => format!("{node}[[\"{label}\"]]"),
                StepKind::Hold { .. } | StepKind::WaitFor { .. } => {
                    format!("{node}{{{{\"{label}\"}}}}")
                }
                StepKind::Approve { .. } => format!("{node}[\"{label}\"]"),
            };
            out.push_str(&format!("  {prev} --> {shape}\n"));
            prev = node;
        }
        out
    }
}

impl Step {
    /// One line a person can read: "Approve: any of alice, bob (from $5,000.00)".
    pub fn describe(&self) -> String {
        match &self.kind {
            StepKind::Approve {
                approvers,
                require,
                min_amount_cents,
            } => {
                let who = match (approvers.is_empty(), require) {
                    (true, _) => "anyone".to_owned(),
                    (false, Require::Any) if approvers.len() == 1 => approvers[0].clone(),
                    (false, Require::Any) => format!("any of {}", approvers.join(", ")),
                    (false, Require::All) => format!("all of {}", approvers.join(", ")),
                };
                match min_amount_cents {
                    Some(c) => format!("Approve: {who} (from {})", dollars(*c)),
                    None => format!("Approve: {who}"),
                }
            }
            StepKind::Hold { days } => {
                format!("Hold {days} day{}", if *days == 1 { "" } else { "s" })
            }
            StepKind::WaitFor { signal, label } => {
                format!("Wait for {}", label.as_deref().unwrap_or(signal))
            }
            StepKind::Pay {
                release: Release::Auto,
                ..
            } => "Pay automatically".to_owned(),
            StepKind::Pay {
                release: Release::Manual,
                by,
            } if by.is_empty() => "Pay when released".to_owned(),
            StepKind::Pay {
                release: Release::Manual,
                by,
            } => format!("Pay when released by {}", by.join(" or ")),
        }
    }
}

pub fn valid_signal(s: &str) -> bool {
    (1..=60).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub fn dollars(cents: Cents) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let c = cents.unsigned_abs();
    let whole = (c / 100).to_string();
    let mut grouped = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}${grouped}.{:02}", c % 100)
}

// ── Evaluation ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub step_id: String,
    pub actor: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    pub key: String,
    pub at: DateTime<Utc>,
}

/// Everything evaluation needs to know about one open bill.
#[derive(Debug, Clone)]
pub struct Facts<'a> {
    pub amount_cents: Cents,
    pub received_at: DateTime<Utc>,
    pub approvals: &'a [Approval],
    pub signals: &'a [Signal],
    pub released_at: Option<DateTime<Utc>>,
    /// A payment for this bill didn't go or came back, so even an automatic
    /// pay step waits for a person to release it.
    pub release_by_person: bool,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StepState {
    Done {
        at: DateTime<Utc>,
    },
    Skipped {
        reason: String,
    },
    /// The step the bill is on now.
    Active,
    /// After the active step.
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    AwaitingApproval {
        step_id: String,
        waiting_on: Vec<String>,
    },
    Held {
        step_id: String,
        until: DateTime<Utc>,
    },
    WaitingFor {
        step_id: String,
        signal: String,
    },
    AwaitingRelease {
        by: Vec<String>,
    },
    ReadyToPay,
    Released {
        at: DateTime<Utc>,
    },
}

impl Status {
    pub fn name(&self) -> &'static str {
        match self {
            Status::AwaitingApproval { .. } => "awaiting_approval",
            Status::Held { .. } => "held",
            Status::WaitingFor { .. } => "waiting_for",
            Status::AwaitingRelease { .. } => "awaiting_release",
            Status::ReadyToPay => "ready_to_pay",
            Status::Released { .. } => "released",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepProgress {
    pub id: String,
    pub describe: String,
    #[serde(flatten)]
    pub state: StepState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Progress {
    #[serde(flatten)]
    pub status: Status,
    pub steps: Vec<StepProgress>,
}

impl Progress {
    /// The step a person can act on now, if any.
    pub fn active_step(&self) -> Option<&str> {
        self.steps
            .iter()
            .find(|s| s.state == StepState::Active)
            .map(|s| s.id.as_str())
    }
}

/// Where a bill stands in its workflow. The workflow must be valid.
///
/// Steps run in order. Each finishes at a moment, and the next starts from
/// it: a hold counts its days from when the step before it finished.
/// Approvals recorded on a later step don't count until the bill gets there,
/// but they aren't lost (the dispatcher refuses them anyway).
pub fn evaluate(workflow: &Workflow, facts: &Facts) -> Progress {
    let mut steps = Vec::with_capacity(workflow.steps.len());
    let mut cursor = facts.received_at;
    let mut status = None;

    for step in &workflow.steps {
        let progress = |state| StepProgress {
            id: step.id.clone(),
            describe: step.describe(),
            state,
        };
        if status.is_some() {
            steps.push(progress(StepState::Pending));
            continue;
        }
        match &step.kind {
            StepKind::Approve {
                approvers,
                require,
                min_amount_cents,
            } => {
                if let Some(min) = min_amount_cents
                    && facts.amount_cents < *min
                {
                    steps.push(progress(StepState::Skipped {
                        reason: format!("under {}", dollars(*min)),
                    }));
                    continue;
                }
                let mine: Vec<&Approval> = facts
                    .approvals
                    .iter()
                    .filter(|a| {
                        a.step_id == step.id
                            && (approvers.is_empty() || approvers.contains(&a.actor))
                    })
                    .collect();
                let done_at = match require {
                    Require::Any => mine.iter().map(|a| a.at).min(),
                    Require::All => {
                        let latest = approvers
                            .iter()
                            .map(|who| mine.iter().filter(|a| &a.actor == who).map(|a| a.at).min())
                            .collect::<Option<Vec<_>>>();
                        latest.and_then(|ats| ats.into_iter().max())
                    }
                };
                match done_at {
                    Some(at) => {
                        cursor = cursor.max(at);
                        steps.push(progress(StepState::Done { at: cursor }));
                    }
                    None => {
                        let waiting_on = match require {
                            Require::Any => approvers.clone(),
                            Require::All => approvers
                                .iter()
                                .filter(|who| !mine.iter().any(|a| &a.actor == *who))
                                .cloned()
                                .collect(),
                        };
                        status = Some(Status::AwaitingApproval {
                            step_id: step.id.clone(),
                            waiting_on,
                        });
                        steps.push(progress(StepState::Active));
                    }
                }
            }
            StepKind::Hold { days } => {
                let until = cursor + Duration::days(i64::from(*days));
                if facts.now >= until {
                    cursor = until;
                    steps.push(progress(StepState::Done { at: until }));
                } else {
                    status = Some(Status::Held {
                        step_id: step.id.clone(),
                        until,
                    });
                    steps.push(progress(StepState::Active));
                }
            }
            StepKind::WaitFor { signal, .. } => {
                match facts
                    .signals
                    .iter()
                    .filter(|s| &s.key == signal)
                    .map(|s| s.at)
                    .min()
                {
                    Some(at) => {
                        cursor = cursor.max(at);
                        steps.push(progress(StepState::Done { at: cursor }));
                    }
                    None => {
                        status = Some(Status::WaitingFor {
                            step_id: step.id.clone(),
                            signal: signal.clone(),
                        });
                        steps.push(progress(StepState::Active));
                    }
                }
            }
            StepKind::Pay { release, by } => match facts.released_at {
                Some(at) => {
                    status = Some(Status::Released { at });
                    steps.push(progress(StepState::Done { at: cursor.max(at) }));
                }
                None => {
                    status = Some(match release {
                        Release::Auto if !facts.release_by_person => Status::ReadyToPay,
                        Release::Auto | Release::Manual => {
                            Status::AwaitingRelease { by: by.clone() }
                        }
                    });
                    steps.push(progress(StepState::Active));
                }
            },
        }
    }
    Progress {
        status: status.expect("a valid workflow ends in a pay step"),
        steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wf(v: serde_json::Value) -> Workflow {
        let w: Workflow = serde_json::from_value(v).unwrap();
        w.validate().unwrap();
        w
    }

    fn t(day: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap() + Duration::days(day)
    }

    fn approval(step: &str, who: &str, day: i64) -> Approval {
        Approval {
            step_id: step.into(),
            actor: who.into(),
            at: t(day),
        }
    }

    fn facts<'a>(
        amount: Cents,
        approvals: &'a [Approval],
        signals: &'a [Signal],
        now: i64,
    ) -> Facts<'a> {
        Facts {
            amount_cents: amount,
            received_at: t(0),
            approvals,
            signals,
            released_at: None,
            release_by_person: false,
            now: t(now),
        }
    }

    /// Quarry-style: controller approves bills from $5,000, the bill waits
    /// until the customer has paid, then a 3-day hold, then auto-pay.
    fn quarry() -> Workflow {
        wf(json!({"steps": [
            {"id": "controller", "type": "approve", "approvers": ["robert"], "min_amount_cents": 500_000},
            {"id": "funded", "type": "wait_for", "signal": "customer_funded", "label": "the customer's payment"},
            {"id": "cooling", "type": "hold", "days": 3},
            {"id": "pay", "type": "pay", "release": "auto"}
        ]}))
    }

    #[test]
    fn small_bill_skips_approval_and_waits_for_funding() {
        let p = evaluate(&quarry(), &facts(10_000, &[], &[], 1));
        assert_eq!(
            p.status,
            Status::WaitingFor {
                step_id: "funded".into(),
                signal: "customer_funded".into()
            }
        );
        assert!(matches!(p.steps[0].state, StepState::Skipped { .. }));
        assert_eq!(p.active_step(), Some("funded"));
    }

    #[test]
    fn hold_counts_from_the_signal_then_pays_automatically() {
        let signals = [Signal {
            key: "customer_funded".into(),
            at: t(5),
        }];
        let held = evaluate(&quarry(), &facts(10_000, &[], &signals, 6));
        assert_eq!(
            held.status,
            Status::Held {
                step_id: "cooling".into(),
                until: t(8)
            }
        );
        let ready = evaluate(&quarry(), &facts(10_000, &[], &signals, 8));
        assert_eq!(ready.status, Status::ReadyToPay);
        // After a payment that didn't go, a person releases it instead.
        let failed = Facts {
            release_by_person: true,
            ..facts(10_000, &[], &signals, 8)
        };
        assert_eq!(
            evaluate(&quarry(), &failed).status,
            Status::AwaitingRelease { by: vec![] }
        );
    }

    #[test]
    fn large_bill_needs_the_named_approver() {
        let stranger = [approval("controller", "mallory", 1)];
        let p = evaluate(&quarry(), &facts(600_000, &stranger, &[], 2));
        assert_eq!(
            p.status,
            Status::AwaitingApproval {
                step_id: "controller".into(),
                waiting_on: vec!["robert".into()]
            }
        );
        let ok = [approval("controller", "robert", 1)];
        let p = evaluate(&quarry(), &facts(600_000, &ok, &[], 2));
        assert_eq!(p.active_step(), Some("funded"));
    }

    #[test]
    fn require_all_lists_who_is_left_and_finishes_at_the_last_signature() {
        let w = wf(json!({"steps": [
            {"id": "two", "type": "approve", "approvers": ["ann", "bob"], "require": "all"},
            {"id": "wait", "type": "hold", "days": 2},
            {"id": "pay", "type": "pay"}
        ]}));
        let one = [approval("two", "ann", 1)];
        let p = evaluate(&w, &facts(100, &one, &[], 9));
        assert_eq!(
            p.status,
            Status::AwaitingApproval {
                step_id: "two".into(),
                waiting_on: vec!["bob".into()]
            }
        );
        let both = [approval("two", "ann", 1), approval("two", "bob", 4)];
        assert_eq!(
            evaluate(&w, &facts(100, &both, &[], 5)).status,
            Status::Held {
                step_id: "wait".into(),
                until: t(6)
            }
        );
        assert_eq!(
            evaluate(&w, &facts(100, &both, &[], 6)).status,
            Status::AwaitingRelease { by: vec![] }
        );
    }

    #[test]
    fn released_bill_is_released() {
        let w = Workflow::builtin_default();
        let mut f = facts(100, &[], &[], 1);
        f.released_at = Some(t(1));
        assert_eq!(evaluate(&w, &f).status, Status::Released { at: t(1) });
    }

    #[test]
    fn validation_catches_shape_errors() {
        let bad = |v: serde_json::Value| {
            serde_json::from_value::<Workflow>(v)
                .unwrap()
                .validate()
                .unwrap_err()
        };
        assert_eq!(bad(json!({"steps": []})), WorkflowError::Empty);
        assert_eq!(
            bad(json!({"steps": [{"id": "a", "type": "hold", "days": 1}]})),
            WorkflowError::PayNotLast
        );
        assert_eq!(
            bad(json!({"steps": [{"id": "p", "type": "pay"}, {"id": "q", "type": "pay"}]})),
            WorkflowError::PayNotLast
        );
        assert_eq!(
            bad(
                json!({"steps": [{"id": "a", "type": "hold", "days": 1}, {"id": "a", "type": "pay"}]})
            ),
            WorkflowError::DuplicateId("a".into())
        );
        assert_eq!(
            bad(
                json!({"steps": [{"id": "x", "type": "approve", "require": "all"}, {"id": "p", "type": "pay"}]})
            ),
            WorkflowError::AllWithoutApprovers("x".into())
        );
        assert_eq!(
            bad(json!({"steps": [{"id": "Bad Id", "type": "pay"}]})),
            WorkflowError::BadId("Bad Id".into())
        );
        assert!(
            serde_json::from_value::<Workflow>(
                json!({"steps": [{"id": "p", "type": "pay", "colour": 1}]})
            )
            .is_err()
        );
    }

    #[test]
    fn describes_and_charts() {
        let w = quarry();
        assert_eq!(w.steps[0].describe(), "Approve: robert (from $5,000.00)");
        assert_eq!(w.steps[1].describe(), "Wait for the customer's payment");
        let m = w.mermaid();
        assert!(m.starts_with("flowchart LR"));
        assert!(m.contains("s3[[\"Pay automatically\"]]"), "{m}");
        assert_eq!(dollars(123_456_789), "$1,234,567.89");
        assert_eq!(dollars(5), "$0.05");
    }
}
