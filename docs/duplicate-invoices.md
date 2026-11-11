# Duplicate and revised invoices: the stack

A vendor sending the same invoice again never creates a second payable bill,
and the resend is never silently discarded. Every arrival is kept and stacked
on the bill it belongs to (`bill_arrivals`, migration
`0024_bill_arrivals.sql`; store: `payvy-store::bill_arrivals`; pure rule:
`payvy_domain::bills::arrival_kind`).

## How a copy is recognized

Duplicate detection is vendor + normalized invoice number
(`payvy_domain::bills::invoice_key`), exactly as `bill.create` has always
enforced it. When `bill.create` finds a live bill with that key and
`allow_duplicate` is not set, the copy stacks instead of being refused:

- Its document, if it was entered from one, is filed with the existing bill.
- A `bill.invoice_received` event records the invoice number, how many times
  it has now been received (`received_count`), the amount this copy carries
  and whether it is pending a decision.
- Same amount as the bill currently carries: a **resend**. Recorded, no
  state change, never payable twice — including a copy that arrives after
  the bill was paid.
- Different amount on an open bill: a **revision**. It becomes the bill's
  one pending revision (a newer revision supersedes an undecided older one)
  and blocks payment until a person decides it. A different amount on a
  closed bill is recorded but never pending: there is nothing left to pay.

The first arrival of every bill is in the stack too, so `received_count`
counts the original. Bills that predate the migration were backfilled with
their first arrival.

## Deciding a revision

`bill.revision_accept` and `bill.revision_reject` are person-only
(`Permission::Prepare`: deciding what an invoice says is preparation, not
approval — the workflow's approval step still follows).

- Accept: the bill takes the arrival's amount and lines; the prior version
  stays in the stack, not payable. Emits `bill.revision_accepted` with the
  old and new amounts. Allocations may not exceed the bill wherever its
  amount changes (Q8): if reserved/consumed allocations exceed the revised
  amount, acceptance is refused until the excess is reversed with
  `po.allocation_reverse` — an explicit record, never silent. A changed
  amount also voids any PO override (its fingerprint no longer matches).
- Reject: the bill keeps its amount. Emits `bill.revision_rejected`.
- Deciding with nothing pending is a conflict, never a bare success.

While a revision is pending, `bill.release` and `bill.auto_pay` are refused
in the same transaction as payment, whatever the PO switch says — the
disputed thing is the amount itself. `bill.mark_paid` only warns
(`revision_warning`), like a PO exception: it records a payment that already
happened.

## Where the count shows

Any bill received more than once carries a `Sent N×` marker — on the bill
page, in bill lists and in Waiting on you (`bill.list` and `inbox.get`
report `received_count`) — with the dates of each arrival one click away in
the bill's **Received N times** stack. A bill with an undecided revision
also shows `Revised` and, in Waiting on you, both amounts, until it is
decided. `bill.get` returns `arrivals` (oldest first; `seq` is the stack
order, since copies can share a timestamp) and `pending_revision`.

`allow_duplicate` remains the deliberate override for genuinely distinct
bills that share a number: it creates a separate bill, and that choice stays
recorded as `duplicate_allowed` on its `bill.created` event.

Matching, allocations and the payment gate always see one bill with one
current amount; the stack is an inbox/bill behaviour, not a matching rule.
