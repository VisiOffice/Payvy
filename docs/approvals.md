# Approvals and separation of duties

Who may approve the step a bill is waiting on. The caller also needs the Approver or Admin role; see [Company roles](roles.md). The rules live in
`crates/payvy-domain/src/duties.rs` (pure, tested) and are applied by `bill.approve`
(`crates/payvy-server/src/commands/bills.rs`). `bill.get`, `bill.list` and `inbox.get` report the
same answer before anyone tries, so the web app only offers Approve when it will work.

## What was enforced before

- `bill.approve` is `person_only`: agent keys are refused and don't see the tool.
- On a step with named approvers, only they may approve; an empty list means any person with approval permission.
- The `approvals` table is unique on `(bill_id, step_id, actor)`, so a person couldn't approve the
  *same* step twice (which matters for `require: all`).
- Nothing else. The person who entered a bill, or uploaded its invoice, could approve it, and one
  person could approve every step of a multi-step workflow one after another (an "anyone" step,
  then a step naming them).

## The rules now

1. **One approval per person per bill. Always on.** A person who approved any step of a bill
   can't approve another step of it. A workflow with two approval steps exists to get two pairs
   of eyes; letting one person fill both makes it a one-person workflow without anyone noticing,
   which is the very thing the second step was added to prevent. It is unconditional because
   there is no case where the same person signing twice adds a control, and a setting for it
   would only let the workflow say something it doesn't do.
   - A person can't approve a step if that would leave a **later** step with nobody allowed to
     approve it (for example an "anyone" step followed by a controller step naming only them).
     The refusal says so and names who else can take the current step. This only refuses when
     it is *this* approval that strands the later step; if the bill was going to get stuck
     anyway, the later step says so when the bill gets there.
   - Workflows whose named approvers can't fill every approval step with a different person are
     refused by `Workflow::validate` (`SamePersonTwice`), and the builder in the web app shows it
     (`needsAPersonTwice` in `web/src/workflow.ts`). Steps anyone may approve are never the
     problem. The check covers every step, including ones with `min_amount_cents`, because a
     large enough bill reaches all of them.
2. **Whoever entered or uploaded a bill can't approve it**, when the company turns on
   `submitter_cannot_approve` (Settings > Approvals, or `approval_rules.set`). It is off by
   default. "Entered" is `bills.created_by` (whoever ran `bill.create`); "uploaded" is
   `documents.created_by` for each document filed with the bill.
   - Agents aren't people: a bill an agent entered from an emailed invoice (uploaded by the agent
     `email`) is approved by anyone, as before.
   - Changing it is `person_only`, appends `approval_rules.changed` (with what it was and the
     optional reason) to the event log, and a change that changes nothing is refused.
   - It applies to every approval from the moment it changes, including bills already waiting.
     That is deliberate: it is a rule about who may sign, not part of the workflow a bill pins.

A refusal is `forbidden` (403), except approving the same step twice, which stays `conflict` (409)
as before. Its message says why and who else can approve: the step's remaining approvers, or for
an "anyone" step the people who sign in (from `people`), less everyone the rules leave out.
If nobody is left, it says the bill can't go further: reject it, or change the workflow and enter
it again (a bill keeps the workflow version it arrived under).

In results, `approval` on a bill waiting for approval is `{ step_id, can_approve, refusal?,
message?, others, anyone }`. `inbox.get` keeps a bill you can't approve in `bills` with
`action: "look"` and leaves it out of `count`; `bill.list` with `mine` leaves it out.

## Decisions made conservatively

- **Identity is the actor name.** Events, approvals and `created_by` all record a name, not a key,
  so the rules compare names. An agent key created with the same actor name as a person (for
  example `key create --actor robert --kind agent`) counts as that person for these rules. Treating
  them as one is the safe direction; give agents their own names.
- **Approvals given before this change still count.** `workflow::evaluate` is unchanged, so a bill
  someone already approved twice isn't sent back. The rules decide only who may approve next.
- **Releasing isn't an approval.** The person who approved a bill (or entered it) may still release
  it at a manual pay step; small teams depend on that, and the pay step's `by` list already
  restricts who releases. Extending separation to release is a possible later rule.
- **Removed people still count as submitters.** A bill entered by someone since removed still
  can't be approved by them (they can't sign in anyway); they aren't offered as "others".
- **The "anyone" pool is active people with the Approver or Admin role.** Person API keys without a sign-in aren't listed as
  "others", but they may still approve.
