# Purchase orders

Purchase orders (PO Matching Spec v2) let a company say what each vendor is authorized to bill
and hold back payment of bills that aren't matched to one. Commands are `po.*` in the registry
(`crates/payvy-domain/src/commands.rs`), implemented in
`crates/payvy-server/src/commands/purchase_orders.rs`.

## The company switch

Purchase orders are **off** until an admin turns them on (`po.configure`, person only, Admin;
Settings > Purchase orders in the app). `po.settings` reports `{enabled, updated_at, updated_by}`.
The setting lives in `po_company_settings`; no row means off. Turning it on or off emits
`po.settings_changed` with `from` and `to`; asking for the state it is already in is refused.

While off:

- Every `po.*` write (create, update, activate, close, cancel, import, set_vendor_settings,
  match_apply, allocation_accept, allocation_reverse, override) is refused with a conflict that
  says an admin can turn them on. Reads (`po.get`, `po.list`, `po.vendor_settings`,
  `po.match_preview`) still work.
- The payment gate does nothing: no bill has a PO exception, so turning purchase orders off never
  blocks a release, auto-pay or mark-paid.
- Rejecting or voiding a bill still releases its allocations, so capacity isn't stranded.
- The app hides the Purchase orders nav entry and the bill page's PO section (`bill.get` reports
  `purchase_orders.enabled`; the signed-in session reports `features.purchase_orders`).

While on, whether a vendor's bills require a PO and its overage caps stay per vendor
(`po.set_vendor_settings`, table `vendor_po_settings`). They apply live, including to bills
already in flight, because the gate is on money leaving rather than on the workflow a bill
pinned.

## Closing and canceling with allocations standing

Closing or canceling (Q6, as proposed in review) stops new allocations — only active
POs take them — but reserved and consumed allocations already made stand, and nothing
is released implicitly. Their bills carry a blocking `ineligible_po` exception until
the allocation is reversed with `po.allocation_reverse` or a person permits the bill
with `po.override`.

## Suspended purchase orders

`po.suspend` (person only, Admin, with a reason) pauses an active PO without ending it:
existing allocations stand and keep counting, new allocations are refused, and bills
allocated to it carry a blocking `suspended_po` exception until `po.reactivate` (person
only, Admin) makes it active again — or a person permits one bill with `po.override`.
Suspending emits `po.suspended` with the reason; reactivating emits `po.reactivated`.
A suspended PO can still be closed or canceled.

## The enumerated policy registry

Vendor/company-specific cases (an invoice larger than its order period, a suspended
order) are data, never core code. Policies live in `vendor_po_policies`, versioned like
workflows: `po.policy_set` (person only, Admin) supersedes the active version with a
new immutable one and emits `po.policy_changed`; asking for what is already in force is
refused. Every bill pins the version in force when it is created
(`bills.policy_version_id`), so a policy change applies to new bills and never rewrites
one in flight. A bill with no pin reads the defaults. `po.policy_get` reports the
version in force (0 is the defaults). Agents may not set policy; agent-proposed policy
changes are a later build.

The first registered policy is `over_period` — an invoice whose date falls outside the
service period of the PO it is allocated to:

- `flag_only` (the default): flagged beside the bill (`notices` in the PO view, and
  `over_period` on `po.match_preview` candidates), never blocks payment.
- `require_full_cover`: the out-of-period allocations do not count as covering the
  bill; the bill carries a blocking `over_period` exception until a person overrides.

A partly covered bill's shortfall is `over_available` when none of the vendor's active
POs has room for the rest (the cumulative caps are the limit), `uncovered` when room
exists and just isn't used. Both are overridable like any other exception; the caps
themselves are unchanged. For override validity the two are one family: the kind flips
as other bills use the vendor's room (or POs are added or closed) without anything
about the bill changing, so an override permits the shortfall either way it is
labelled today. Its fingerprint binds to the family name `uncovered`, which also
keeps overrides written before `over_available` existed valid.

## Quantity matching

Bill lines carry the quantity, unit and unit price as printed (from extraction's
line details, the invoice review, or `bill.create` input); PO lines already stored
theirs. Quantities are exact micro-units — no float enters the arithmetic
(`payvy-domain::quantity`, `invoice_evidence::quantity_micro`).

A bill line belongs to a PO line by normalized description, with the unit agreeing
when both sides state one. More than one fit is `ambiguous_line`, none is
`unmatched_line`; both are identification findings and never silently map a line.
Against that mapping, quantities are cumulative across every live allocation to
the PO: billing past the ordered quantity is `quantity_over`, and a billed rate
one cent off the PO line's rate is `rate_mismatch` (exact to the cent).

The registry's `quantity_matching` policy decides what findings do, under the
version the bill pinned: `flag_only` (default) shows them beside the bill — with
ordered/billed/remaining per line and on `po.match_preview` candidates — and
never blocks; `enforce` makes `quantity_over` and `rate_mismatch` block payment
like other exceptions (overridable). Lines without quantities, on either side,
are amount-only and not quantity-checked.

## Overrides and separation of duties

`po.override` permits one bill's PO exception (Confirmed tier, person only, Admin), bound to the
bill, its current allocations, the exception and an authorized excess. Two rules always apply
(`payvy-domain/src/duties.rs`):

1. **The preparer can't override.** Whoever entered the bill (`created_by`) or uploaded a document
   filed with it can't permit its exception — the same people the "submitter can't approve" rule
   names. `bill.get` reports `purchase_orders.can_override` so the app only offers it when it
   will work.
2. **The overrider can't release.** When a bill is payable only because of a currently valid
   override, its author can't `bill.release` it; someone else with release permission must.
   `bill.get`'s `can_release` is false for them, and workflow notifications don't ask them to
   release it. If the override no longer applies (the bill was matched, or the exception
   changed), there is no restriction. Auto-pay is unaffected (no person releases), and
   `bill.mark_paid` still only warns about an open exception, since it records a payment that
   already happened.
