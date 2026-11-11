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
