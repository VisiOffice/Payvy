# Hosted plans and entitlements

Hosted Payvy sells two flat plans per company. The plan decides two things, and the core
enforces both on the server for the browser, the API, MCP and the workers:

| Entitlement | Gates | Bring your own agent ($39) | Payvy-managed agent ($99) |
| --- | --- | --- | --- |
| `reader` | Every paid model call: `document.extract` (direct, retried or a split part), the background reader, and the reads behind *Create N bills* | off | on |
| `payments` | Every new payment through the bank: `bill.release` with a paying bank, `bill.auto_pay`, and `payments::submit` | on | on |

Self-hosted Payvy has no plans: nothing is checked and everything is on.

## Mode

`PAYVY_HOSTED=true` turns enforcement on. It needs `PAYVY_OPERATOR_KEY` (startup fails
without it), because only the operator can grant a plan. The mode is set explicitly and never
inferred from which secrets are present.

In hosted mode a company with no stored entitlement is **provisional**: reader off, payments
off. `org.get` returns the company's `plan` (`hosted`, `provisional`, `reader`, `payments`) so
the app can say so up front; the server still checks on every command.

## Setting a plan

Only the operator command `entitlement.set` (`/operator/commands/entitlement.set`, or
`payvy org entitle --org … --reader … --payments … --version …`) changes a plan:

- The `version` must be higher than the stored one. The management app uses its own ordering
  (for example the Stripe event's time), so a late or reordered call can't undo a newer one.
- The same version with the same values is a replay: `changed: false`, no event.
- A version no higher than the stored one, with different values, is refused (409).
- Every change appends `entitlement.set` with the old and new values, by `operator`.
- `entitlement.get` (or `payvy org entitlement --org …`) shows what is stored and in force.

An outage of the management app changes nothing: only an explicit `entitlement.set` revokes.

## What a lapse never stops

- Signing in, reading, exporting, and the audit history.
- `payment.refresh`, bank webhooks and the payment checker, so settlements and ACH returns
  still land.
- Resending an attempt that is already `submitting`, with its own idempotency key.
- `bill.release` with `pay_elsewhere: true`, then `bill.mark_paid`.
- Ledger posting.
- Bring-your-own-agent work: upload, download, review, and `bill.create`.

When the reader is switched on (`reader_since`), the background reader reads only documents
received from then on. Documents left unread under a plan without it, the backlog, are read
only when someone asks (`document.extract`), so an upgrade never reads a backlog at Payvy's cost
unasked. Staying on keeps the time; switching off clears it.

The plan is read inside each command's own transaction, so a confirmation token issued before a
lapse is refused when it is committed after it. The payment worker skips a company without
`payments`; its bills stay `ready_to_pay` and are never sent to a person for it. The background
reader skips a company without `reader`.

## Signing up

The management app's signup page calls the operator command `org.create` with
`send_sign_in_link: true`. The core creates the company and its first Admin, then emails one
sign-in link that opens in the new company. Opening it is what proves the address; the Rails app
never signs anyone in. The link is sent only when the company is created, never on a replay
(the result's `sign_in_link` is `sent`, `not_sent` or `not_configured`).

A person whose address belongs to several companies gets one link. Opening it lists only those
companies, and they pick one; the company menu switches between them later. Each choice and
switch checks membership at that moment, and a session belongs to one company.

## Turning hosted mode on (rollout)

Before setting `PAYVY_HOSTED=true` on an install with companies already in use, give each of
them an explicit entitlement, or the first deploy turns their reader and payments off:

```bash
payvy org list
payvy org entitle --org <id> --reader true --payments true --version 1
```
