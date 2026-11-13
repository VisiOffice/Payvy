# Hosted plans and entitlements

Hosted Payvy sells two flat plans per company. The plan decides two things, and the core
enforces both on the server for the browser, the API, MCP and the workers:

| Entitlement | Gates | Bring your own agent (`byoa`) | Payvy reads for you (`managed`) |
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
(the result's `sign_in_link` is `sent`, `not_sent`, `not_configured` or `failed`). `sent` means
queued, not delivered. On anything but `sent` the company still exists: the signup page tells the
person to ask for a link on the sign-in page, and never creates the company again.

Anyone can sign up with someone else's address. They get no access, but the owner of the address
gets mail and an extra company in their chooser. The signup app rate-limits signups, and hides or
removes companies whose link was never opened.

A person whose address belongs to several companies gets one link. Opening it lists only those
companies, and they pick one; the company menu switches between them later. Each choice and
switch checks membership at that moment, and a session belongs to one company.

## Billing (Settings > Billing)

Plans are paid through Stripe, which the private management app owns: prices, Checkout, the
customer portal, webhooks and the plan policy. The core keeps no Stripe state. It asks the
management app, server to server, with `PAYVY_BILLING_URL` and `PAYVY_BILLING_KEY`:

- `billing.get` shows the subscription as the management app last saw it, the plans it offers
  (name, price, description, trial days) and the plan in force.
- `billing.checkout {plan: byoa|managed}` returns a Checkout link. A company that already
  subscribes gets the portal instead, never a second subscription.
- `billing.portal` returns the portal link, to change plan or card, see invoices or cancel.

Prices and terms (trial length, the past-due grace and the date payments stop) live only in the
management app; the open-source app shows what `billing.get` returns and states none of its
own. The core passes on only the known fields of the management app's answer, so nothing else
it sends (a link, a Stripe id) reaches the page or an agent. If the management app can't be
reached, `billing.get` still answers, with `available: false` and the plan in force. The app
shows Settings > Billing only when `me.plan.billing` is true (hosted, with both settings).

`billing.get` is a shared read, open to agents too: it carries no links. The other two are
person-only and need Admin. The company id comes from the session and the
email from the person asking; the return URL is `PAYVY_PUBLIC_URL/settings/billing`. A link that
isn't https is refused before it reaches the browser. Opening a
link appends `billing.checkout_opened` or `billing.portal_opened`, without the link, which is a
bearer URL for the company's billing. Nothing here changes what a company may do: the plan
changes only when Stripe confirms and the management app calls `entitlement.set`. Preview asks
nothing. Self-hosted Payvy, or a hosted install without the two settings, answers
`not_configured`; a refusal or an outage of the management app is `billing_error` (502).

## Turning hosted mode on (rollout)

Before setting `PAYVY_HOSTED=true` on an install with companies already in use, give each of
them an explicit entitlement, or the first deploy turns their reader and payments off:

```bash
payvy org list
payvy org entitle --org <id> --reader true --payments true --version 1
```
