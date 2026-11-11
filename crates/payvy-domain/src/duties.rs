//! Separation of duties: who may give the approval a bill is waiting for.
//!
//! Two rules, on top of a step's own list of approvers:
//!
//! - **One approval per person per bill, always.** A workflow with two
//!   approval steps asks for two pairs of eyes; one person filling both
//!   would quietly make it a one-person workflow. So a person who approved
//!   any step of a bill can't approve another, and a person can't approve a
//!   step if that would leave a later step with nobody allowed to approve it.
//!   [`Workflow::validate`](crate::workflow::Workflow::validate) refuses a
//!   workflow whose named approvers can't fill every step with a different
//!   person, so no bill starts down a path it can't finish.
//! - **The submitter doesn't approve**, when the organization turns it on
//!   ([`Rules::submitter_cannot_approve`]): whoever entered the bill or
//!   uploaded its document can't approve it. Agents aren't people, so a bill
//!   an agent entered from an emailed invoice is unaffected.
//!
//! People are compared by actor name, the identity the audit trail records.
//! Approvals already given before these rules existed still count; the rules
//! decide only who may approve next.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::Cents;
use crate::workflow::{Approval, Require, Status, StepKind, Workflow};

/// The organization's approval rules. Default: everything off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Rules {
    pub submitter_cannot_approve: bool,
}

/// What is known about one bill waiting for approval.
#[derive(Debug, Clone)]
pub struct Context<'a> {
    pub workflow: &'a Workflow,
    pub status: &'a Status,
    pub amount_cents: Cents,
    pub approvals: &'a [Approval],
    /// Who entered the bill (`bill.create`).
    pub entered_by: &'a str,
    /// Who uploaded the documents filed with it.
    pub uploaded_by: &'a [String],
    /// Everyone who signs in, for steps any person may approve.
    pub people: &'a [String],
    pub rules: Rules,
}

/// Why someone can't approve the step a bill is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Refusal {
    NotAPerson,
    NotAnApprover { approvers: Vec<String> },
    AlreadyApprovedStep,
    AlreadyApprovedBill { step_id: String },
    Entered,
    Uploaded,
    LastApprover { later_step_id: String },
}

/// Whether one actor can approve the step a bill is on, and who else can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Turn {
    pub step_id: String,
    pub can_approve: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
    /// A sentence for people when `can_approve` is false: why, and who else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Who else may approve this step now. For a step open to any person,
    /// the people who sign in, less those the rules leave out.
    pub others: Vec<String>,
    /// The step is open to any person, not a named list.
    pub anyone: bool,
}

/// One approval a workflow needs, and who could give it.
struct Slot<'a> {
    step_id: &'a str,
    candidates: Vec<&'a str>,
}

/// The slots an approve step adds: one for "any of", one per person for
/// "all of", none when any person may approve (there is always someone).
fn slots_of<'a>(step_id: &'a str, approvers: &'a [String], require: Require) -> Vec<Slot<'a>> {
    if approvers.is_empty() {
        return vec![];
    }
    match require {
        Require::Any => vec![Slot {
            step_id,
            candidates: approvers.iter().map(String::as_str).collect(),
        }],
        Require::All => approvers
            .iter()
            .map(|a| Slot {
                step_id,
                candidates: vec![a.as_str()],
            })
            .collect(),
    }
}

/// Gives each slot a different person, in order, leaving out `excluded`.
/// The step of the first slot that can't have one, if any.
fn first_unfilled<'a>(slots: &[Slot<'a>], excluded: &HashSet<&str>) -> Option<&'a str> {
    fn assign<'a>(
        i: usize,
        slots: &[Slot<'a>],
        excluded: &HashSet<&str>,
        owner: &mut HashMap<&'a str, usize>,
        seen: &mut HashSet<&'a str>,
    ) -> bool {
        for &p in &slots[i].candidates {
            if excluded.contains(p) || !seen.insert(p) {
                continue;
            }
            let free = match owner.get(p) {
                None => true,
                Some(&j) => assign(j, slots, excluded, owner, seen),
            };
            if free {
                owner.insert(p, i);
                return true;
            }
        }
        false
    }
    let mut owner = HashMap::new();
    for i in 0..slots.len() {
        if !assign(i, slots, excluded, &mut owner, &mut HashSet::new()) {
            return Some(slots[i].step_id);
        }
    }
    None
}

/// For [`Workflow::validate`]: the first approve step whose named approvers
/// can't be a different person from those the steps before it need.
pub(crate) fn needs_a_person_twice(workflow: &Workflow) -> Option<String> {
    let slots: Vec<Slot> = workflow
        .steps
        .iter()
        .flat_map(|s| match &s.kind {
            StepKind::Approve {
                approvers, require, ..
            } => slots_of(&s.id, approvers, *require),
            _ => vec![],
        })
        .collect();
    first_unfilled(&slots, &HashSet::new()).map(str::to_owned)
}

/// Whether `who` may approve the step the bill is on now. None unless the
/// bill is waiting for approval.
pub fn turn(cx: &Context, who: &str, is_person: bool) -> Option<Turn> {
    let Status::AwaitingApproval {
        step_id,
        waiting_on,
    } = cx.status
    else {
        return None;
    };
    let index = cx.workflow.steps.iter().position(|s| &s.id == step_id)?;
    let StepKind::Approve {
        approvers, require, ..
    } = &cx.workflow.steps[index].kind
    else {
        return None;
    };

    // Left out for everyone: people who already approved this bill, and,
    // with the rule on, whoever entered it or uploaded its document. (On an
    // "all of" step, approving this same step doesn't leave someone out;
    // they are simply no longer waited on.)
    let mut excluded: HashSet<&str> = cx
        .approvals
        .iter()
        .filter(|a| &a.step_id != step_id)
        .map(|a| a.actor.as_str())
        .collect();
    if cx.rules.submitter_cannot_approve {
        excluded.insert(cx.entered_by);
        excluded.extend(cx.uploaded_by.iter().map(String::as_str));
    }

    let anyone = approvers.is_empty();
    let pool: Vec<&str> = match (anyone, require) {
        (true, _) => cx.people.iter().map(String::as_str).collect(),
        (false, Require::Any) => approvers.iter().map(String::as_str).collect(),
        (false, Require::All) => waiting_on.iter().map(String::as_str).collect(),
    };
    let others: Vec<String> = pool
        .into_iter()
        .filter(|p| *p != who && !excluded.contains(p))
        .map(str::to_owned)
        .collect();

    let refusal = refusal(cx, who, is_person, index, &excluded);
    let message = refusal
        .as_ref()
        .map(|r| format!("{}. {}", reason(r, step_id), who_else(&others, anyone)));
    Some(Turn {
        step_id: step_id.clone(),
        can_approve: refusal.is_none(),
        refusal,
        message,
        others,
        anyone,
    })
}

fn refusal(
    cx: &Context,
    who: &str,
    is_person: bool,
    index: usize,
    excluded: &HashSet<&str>,
) -> Option<Refusal> {
    let step = &cx.workflow.steps[index];
    let StepKind::Approve { approvers, .. } = &step.kind else {
        return None;
    };
    if !is_person {
        return Some(Refusal::NotAPerson);
    }
    if !approvers.is_empty() && !approvers.iter().any(|a| a == who) {
        return Some(Refusal::NotAnApprover {
            approvers: approvers.clone(),
        });
    }
    if let Some(a) = cx.approvals.iter().find(|a| a.actor == who) {
        return Some(if a.step_id == step.id {
            Refusal::AlreadyApprovedStep
        } else {
            Refusal::AlreadyApprovedBill {
                step_id: a.step_id.clone(),
            }
        });
    }
    if cx.rules.submitter_cannot_approve {
        if cx.entered_by == who {
            return Some(Refusal::Entered);
        }
        if cx.uploaded_by.iter().any(|u| u == who) {
            return Some(Refusal::Uploaded);
        }
    }

    // Would approving here leave a later step without anyone? Only refuse
    // when it is this approval that does it: if the bill was going to get
    // stuck anyway, that step says so when the bill reaches it.
    let mut slots = Vec::new();
    if let (
        Status::AwaitingApproval { waiting_on, .. },
        StepKind::Approve {
            require: Require::All,
            ..
        },
    ) = (cx.status, &step.kind)
    {
        // The rest of this step's signatures are still needed.
        slots.extend(waiting_on.iter().filter(|w| *w != who).map(|w| Slot {
            step_id: &step.id,
            candidates: vec![w.as_str()],
        }));
    }
    for later in &cx.workflow.steps[index + 1..] {
        if let StepKind::Approve {
            approvers,
            require,
            min_amount_cents,
        } = &later.kind
            && min_amount_cents.is_none_or(|m| cx.amount_cents >= m)
        {
            slots.extend(slots_of(&later.id, approvers, *require));
        }
    }
    if first_unfilled(&slots, excluded).is_some() {
        return None;
    }
    let mut without_me = excluded.clone();
    without_me.insert(who);
    first_unfilled(&slots, &without_me).map(|s| Refusal::LastApprover {
        later_step_id: s.to_owned(),
    })
}

fn reason(r: &Refusal, step_id: &str) -> String {
    match r {
        Refusal::NotAPerson => "only a person can approve a bill".into(),
        Refusal::NotAnApprover { approvers } => format!(
            "you aren't an approver on step {step_id}; it needs {}",
            approvers.join(" or ")
        ),
        Refusal::AlreadyApprovedStep => format!("you already approved step {step_id}"),
        Refusal::AlreadyApprovedBill { step_id: earlier } => format!(
            "you already approved this bill at step {earlier}, and one person gives a bill one approval"
        ),
        Refusal::Entered => "you entered this bill, and this company has someone other than \
             whoever entered or uploaded a bill approve it"
            .into(),
        Refusal::Uploaded => "you uploaded this bill's document, and this company has someone \
             other than whoever entered or uploaded a bill approve it"
            .into(),
        Refusal::LastApprover { later_step_id } => format!(
            "step {later_step_id} needs you later, and one person gives a bill one approval, \
             so someone else approves step {step_id}"
        ),
    }
}

fn who_else(others: &[String], anyone: bool) -> String {
    match (others, anyone) {
        ([], true) => "Any other person can approve it.".into(),
        (o, true) => format!("Any other person can approve it, such as {}.", list(o)),
        ([], false) => "Nobody else can approve this step, so the bill can't go further: \
             reject it, or change the workflow and enter it again."
            .into(),
        (o, false) => format!("It's waiting on {}.", list(o)),
    }
}

/// "a", "a or b", "a, b or c".
fn list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::{Facts, evaluate};
    use chrono::{DateTime, Utc};
    use serde_json::json;

    fn wf(steps: serde_json::Value) -> Workflow {
        serde_json::from_value(json!({ "steps": steps })).unwrap()
    }

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    fn approval(step: &str, who: &str) -> Approval {
        Approval {
            step_id: step.into(),
            actor: who.into(),
            at: at(),
        }
    }

    struct Bill {
        workflow: Workflow,
        approvals: Vec<Approval>,
        entered_by: String,
        uploaded_by: Vec<String>,
        people: Vec<String>,
        rules: Rules,
        amount: Cents,
    }

    impl Bill {
        fn new(workflow: Workflow) -> Self {
            Self {
                workflow,
                approvals: vec![],
                entered_by: "claude-ap".into(),
                uploaded_by: vec![],
                people: vec!["ann".into(), "bob".into(), "cy".into()],
                rules: Rules::default(),
                amount: 100_000,
            }
        }

        fn turn(&self, who: &str) -> Option<Turn> {
            let status = evaluate(
                &self.workflow,
                &Facts {
                    amount_cents: self.amount,
                    received_at: at(),
                    approvals: &self.approvals,
                    signals: &[],
                    released_at: None,
                    release_by_person: false,
                    now: at(),
                    auto_pay_limit: crate::workflow::AutoPayLimit::effective(None, None),
                },
            )
            .status;
            turn(
                &Context {
                    workflow: &self.workflow,
                    status: &status,
                    amount_cents: self.amount,
                    approvals: &self.approvals,
                    entered_by: &self.entered_by,
                    uploaded_by: &self.uploaded_by,
                    people: &self.people,
                    rules: self.rules,
                },
                who,
                true,
            )
        }
    }

    fn two_steps() -> Workflow {
        wf(json!([
            {"id": "first", "type": "approve"},
            {"id": "second", "type": "approve"},
            {"id": "pay", "type": "pay"}
        ]))
    }

    #[test]
    fn an_agents_bill_can_be_approved_by_anyone_even_with_the_rule_on() {
        let mut b = Bill::new(two_steps());
        b.rules.submitter_cannot_approve = true;
        let t = b.turn("ann").unwrap();
        assert!(t.can_approve, "{t:?}");
        assert_eq!(t.others, ["bob", "cy"]);
        assert!(t.anyone);
    }

    #[test]
    fn whoever_entered_or_uploaded_it_cant_approve_when_the_rule_is_on() {
        let mut b = Bill::new(two_steps());
        b.entered_by = "ann".into();
        b.uploaded_by = vec!["bob".into()];
        assert!(b.turn("ann").unwrap().can_approve, "off by default");

        b.rules.submitter_cannot_approve = true;
        let t = b.turn("ann").unwrap();
        assert_eq!(t.refusal, Some(Refusal::Entered));
        assert_eq!(t.others, ["cy"]);
        assert!(
            t.message.as_deref().unwrap().ends_with("such as cy."),
            "{t:?}"
        );
        assert_eq!(b.turn("bob").unwrap().refusal, Some(Refusal::Uploaded));
        assert!(b.turn("cy").unwrap().can_approve);
    }

    #[test]
    fn one_person_gives_one_approval_per_bill() {
        let mut b = Bill::new(two_steps());
        b.approvals.push(approval("first", "ann"));
        let t = b.turn("ann").unwrap();
        assert_eq!(t.step_id, "second");
        assert_eq!(
            t.refusal,
            Some(Refusal::AlreadyApprovedBill {
                step_id: "first".into()
            })
        );
        assert_eq!(t.others, ["bob", "cy"]);
        assert!(b.turn("bob").unwrap().can_approve);
    }

    #[test]
    fn all_of_step_lists_who_is_left() {
        let mut b = Bill::new(wf(json!([
            {"id": "both", "type": "approve", "approvers": ["ann", "bob"], "require": "all"},
            {"id": "pay", "type": "pay"}
        ])));
        b.approvals.push(approval("both", "ann"));
        assert_eq!(
            b.turn("ann").unwrap().refusal,
            Some(Refusal::AlreadyApprovedStep)
        );
        assert!(b.turn("bob").unwrap().can_approve);
    }

    #[test]
    fn named_step_with_nobody_left_says_so() {
        let mut b = Bill::new(wf(json!([
            {"id": "ok", "type": "approve", "approvers": ["ann"]},
            {"id": "pay", "type": "pay"}
        ])));
        b.entered_by = "ann".into();
        b.rules.submitter_cannot_approve = true;
        let t = b.turn("ann").unwrap();
        assert!(t.others.is_empty());
        assert!(
            t.message
                .unwrap()
                .contains("Nobody else can approve this step")
        );
        let t = b.turn("bob").unwrap();
        assert!(matches!(t.refusal, Some(Refusal::NotAnApprover { .. })));
    }

    #[test]
    fn approving_an_open_step_doesnt_use_up_someone_a_later_step_needs() {
        let mut b = Bill::new(wf(json!([
            {"id": "anyone", "type": "approve"},
            {"id": "controller", "type": "approve", "approvers": ["ann"], "min_amount_cents": 50_000},
            {"id": "pay", "type": "pay"}
        ])));
        let t = b.turn("ann").unwrap();
        assert_eq!(
            t.refusal,
            Some(Refusal::LastApprover {
                later_step_id: "controller".into()
            })
        );
        assert_eq!(t.others, ["bob", "cy"]);
        assert!(b.turn("bob").unwrap().can_approve);
        // Under the controller's threshold that step is skipped: ann may.
        b.amount = 10_000;
        assert!(b.turn("ann").unwrap().can_approve);
    }

    #[test]
    fn a_later_step_stuck_anyway_doesnt_block_someone_else() {
        // ann entered it and is the only controller: the bill will stop
        // there whoever approves first, and that step will say so.
        let mut b = Bill::new(wf(json!([
            {"id": "anyone", "type": "approve"},
            {"id": "controller", "type": "approve", "approvers": ["ann"]},
            {"id": "pay", "type": "pay"}
        ])));
        b.entered_by = "ann".into();
        b.rules.submitter_cannot_approve = true;
        assert!(b.turn("bob").unwrap().can_approve);
    }

    #[test]
    fn agents_cant_approve() {
        let b = Bill::new(two_steps());
        let status = Status::AwaitingApproval {
            step_id: "first".into(),
            waiting_on: vec![],
        };
        let t = turn(
            &Context {
                workflow: &b.workflow,
                status: &status,
                amount_cents: 1,
                approvals: &[],
                entered_by: "claude-ap",
                uploaded_by: &[],
                people: &b.people,
                rules: Rules::default(),
            },
            "claude-ap",
            false,
        )
        .unwrap();
        assert_eq!(t.refusal, Some(Refusal::NotAPerson));
        assert_eq!(t.others, ["ann", "bob", "cy"]);
    }

    #[test]
    fn workflows_must_be_fillable_by_different_people() {
        let ok = wf(json!([
            {"id": "a", "type": "approve", "approvers": ["ann", "bob"]},
            {"id": "b", "type": "approve", "approvers": ["ann"]},
            {"id": "c", "type": "approve"},
            {"id": "pay", "type": "pay"}
        ]));
        assert_eq!(needs_a_person_twice(&ok), None);
        let twice = wf(json!([
            {"id": "a", "type": "approve", "approvers": ["ann"]},
            {"id": "b", "type": "approve", "approvers": ["ann", "bob"], "require": "all"},
            {"id": "pay", "type": "pay"}
        ]));
        assert_eq!(needs_a_person_twice(&twice), Some("b".into()));
    }

    #[test]
    fn lists_read_like_english() {
        let l = |v: &[&str]| list(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(l(&["a"]), "a");
        assert_eq!(l(&["a", "b"]), "a or b");
        assert_eq!(l(&["a", "b", "c"]), "a, b or c");
    }
}
