# Paying bills through your bank

Payvy never holds or moves money. When a person releases a bill, Payvy asks **your** bank to pay
it, then watches the bank until the payment has gone. Mercury is the first bank; Ramp, Brex, Wise
and Airwallex fit the same shape (`payvy-pay`).

## Setup

1. Set `PAYVY_SECRETS_KEY` on the server (`openssl rand -hex 32`). Bank tokens are sealed with it
   (XChaCha20-Poly1305, bound to the organization and connection), so the database alone can't
   pay anyone. Without the key, bank commands say banks aren't set up. **Losing the key means
   reconnecting every bank**, so keep it with your other secrets.
2. In Mercury, create an API token (Settings → API tokens) and pick how payments are sent:

   | Mode | Mercury token | What happens on release |
   |---|---|---|
   | `request` (default) | Custom token with the **RequestSendMoney** scope | The payment waits in Mercury until a Mercury admin approves it. The approver can't be the person who made the token. No IP allowlist needed. |
   | `direct` | Read & Write token | Mercury sends it at once. Mercury requires the server's IP address on the token's allowlist. |

   Request mode gives a second sign-off in the bank itself; direct mode is for teams that want
   Payvy's approval workflow to be the only one.
3. A person connects it:
   ```
   bank.connect { "provider": "mercury", "token": "secret-token:…", "mode": "request" }
   ```
   Payvy checks the token by listing your accounts. With more than one active account it asks
   which one bills are paid from (`account_id`). Use `"environment": "sandbox"` with a Mercury
   sandbox token to try it out.

## Who gets paid

Payvy doesn't store bank account numbers. Each vendor is linked to a **recipient you already set
up in Mercury**, and Payvy keeps only that recipient's id, the method, and what people see: the
last four digits, or the check's city.

```
bank.recipients     { "search": "king" }
vendor.propose_payee { "vendor_id": "…", "recipient_id": "…", "method": "ach" }
vendor.accept_payee  { "proposal_id": "…" }
```

Anyone, agents included, can propose a payee. **Only a person can accept one**, and the event log
records the payee it replaced. Changing where a vendor is paid is the classic AP fraud, so it
always gets a human look. Methods: `ach`, `check`, `domestic_wire`, `real_time_payment`.

## Inviting a vendor to supply payment details

On the vendor page, choose **Invite payment details…**, enter the email and method,
review the invitation, then send it. Mercury sends the email and collects the
banking details. Payvy never stores the onboarding link or the vendor's account
or routing numbers. The connected token needs permission to create and read
recipient invites and read recipients; payment-request permission alone is not
sufficient for this flow.

The API uses the same commands as the page:

```
vendor.invite_payee { "vendor_id": "…", "email": "vendor@example.com", "method": "ach" }
```

This is a **person-only, confirmed** command: preview sends nothing, and commit
uses the preview's single-use token. Supported methods are `ach`, `check`,
`domestic_wire`, and `real_time_payment`. It invites a new recipient; it does not
change an existing Mercury recipient's details or ask for tax documents.

Payvy checks open invitations with Mercury every five minutes, as the agent
`bank`, through the same command the vendor page's **Check now** button and
any agent can run:

```
vendor.refresh_payee_invite { "invite_id": "…" }
vendor.accept_payee         { "proposal_id": "…" }
```

The checker skips invitations whose delivery is uncertain and ones on a
replaced bank connection; both need a person. When Mercury reports completion and the recipient is
active with the requested method, refresh creates one payee proposal. Review
and accept it in **Waiting on you** before it can be used to pay. Refresh previews
roll back the proposal; repeated or concurrent refreshes cannot create another.
Expired and declined invitations can be replaced with a new invitation. A bank
connection change requires a new invitation on that connection.

Mercury does not document an idempotency key for sending invitations. Before the
POST, Payvy commits an `unknown` attempt and consumes its confirmation. If the
process stops, the response is lost, or the final database write fails, the
attempt stays visible and blocks another invite for that vendor and connection.
It is **never automatically resent**. A definite bank refusal is recorded as
`failed` and permits another attempt after fixing the cause.

For uncertain delivery, find the invitation in Mercury and enter its ID on the
vendor page, or run:

```
vendor.refresh_payee_invite { "invite_id": "…", "mercury_invite_id": "…" }
```

Only a person may recover an invitation this way. Recovery checks the email and requested method and never sends another email.
If no invitation can be found, leave the uncertain attempt for investigation;
you can still set up a recipient in Mercury and use the existing
`vendor.propose_payee` → `vendor.accept_payee` flow. Payvy does not infer that an
invitation was never sent from a missing response.

Mercury's [invite endpoint](https://docs.mercury.com/reference/createrecipientinvite)
and [invite status endpoint](https://docs.mercury.com/reference/getrecipientinvite)
define this flow (checked 2026-11-10). Tests use a local fake bank, never a live
account. A debug server can point **only sandbox** connections at an HTTP
loopback fake with `PAYVY_MERCURY_SANDBOX_URL` (and Rho with `PAYVY_RHO_SANDBOX_URL`);
a release build refuses to start with either set.

## Paying

`bill.release` previews the payment (bank, account, recipient, last four, method, amount) and
commits with the confirm token. Once a bank is connected, a bill can't be released without an
accepted payee, unless the release says `"pay_elsewhere": true` (then `bill.mark_paid` records it
when it's paid some other way).

The payment checker asks the bank about every open payment each minute (`payment.refresh` runs
the same check on demand; `payment.list` shows them):

| Bank says | Payment | Bill |
|---|---|---|
| Waiting for approval in Mercury | `awaiting_bank_approval` | released |
| Approved / in progress | `pending` | released |
| Sent | `sent` | **paid**, with the bank's transaction id as the reference |
| Failed, cancelled, rejected | `failed` / `cancelled` / `rejected` | back to waiting for release, with the reason |
| ACH returned (watched for 14 days after sending) | `returned` | reopened, waiting for release |

While a payment is open, the bill can't be marked paid by hand, rejected or voided. Cancel it in
the bank first.

### Paying automatically

When a bill reaches a workflow `pay` step set to `auto` (status `ready_to_pay`), the payment
worker pays it within a minute: `bill.auto_pay`, run as the agent `workflow`, releases the bill
and sends the payment exactly as a release would (same plan, same idempotency key rule), with
the event `bill.auto_released`. The authority is the workflow a person accepted, so any key may
run `bill.auto_pay` to pay a ready bill now; it pays nothing a person hasn't set to pay
automatically. It needs:

- a live Mercury connection (with Rho only, or no bank, the bill waits to be paid elsewhere and
  marked paid, as before), and
- an accepted payee for the vendor at that connection (without one the bill waits; nothing is
  sent).

In `request` mode the payment still waits for a Mercury admin's approval.

A payment that fails, is cancelled or rejected, or comes back as an ACH return sets the bill's
`release_by_person`: from then on its automatic pay step waits for a person to release it
(`awaiting_release`, shown in Waiting on you). Auto-pay never retries a bill. This matters for
recurring identical bills, which Mercury's 24-hour duplicate rule refuses (below).

## Never paying twice

Every attempt has an idempotency key (`payvy-{bill}-{attempt}`) that Mercury honours. If the bank
doesn't answer clearly (a timeout, a 5xx), the payment stays `submitting` and the checker sends it
again **with the same key**: Mercury returns the payment it already made instead of making a
second. A definite refusal fails the attempt; the next release is a new attempt with a new key.

Mercury also refuses a payment that matches one to the same recipient, amount and method in the
last 24 hours. Payvy shows that as a failed payment with Mercury's reason.

## Rho: read-only visibility

Rho's API (v1) only reads: accounts and transactions. So a Rho connection
coexists with the paying bank — it never replaces it, holds no payees, and
cannot pay. It answers "what's in the bank, and what moved":

1. In Rho, create an API Access Token (Settings → API → Access Tokens) with
   `accounts:read` and `transactions:read` only. It's shown once.
2. A person connects it: `bank.connect { "provider": "rho", "token": "rhobat_…" }`
   (`"environment": "sandbox"` against Rho's sandbox, which takes any token).
   Payvy seals the token like Mercury's. With several Rho accounts, the result
   names the display account it picked (the first, unless `account_id` chose one).
3. Anyone with a key can then call `bank.accounts` (balances, in cents) and
   `bank.transactions` (signed cents — negative is money out — with
   counterparty, status and posted time; filter by account, dates, search,
   limit). Amounts come from Rho as integer minor units, unchanged.
4. `bill.release` with only Rho connected behaves as with no paying bank:
   Payvy won't send the payment — pay it another way, then `bill.mark_paid`.
   `bank.disconnect` with `"provider": "rho"` removes just Rho; the default
   removes the paying bank.

Rho tokens expire after 45 days unused (a year at most). When one lapses,
reads fail with a clear "Rho refused this token" and a person reconnects.
Stored transaction sync and matching bank transactions to bills are not
built yet — reads are live from Rho each time.

## Not yet

- An amount ceiling on automatic payments (today the controls are the workflow a person accepted,
  any approval steps before it, and Mercury's own approval in `request` mode).
- Direct recipient creation inside Payvy; invitations are supported.
- Bills in other currencies, and Wise and Airwallex for international payments.
- Ramp and Brex.
- Paying through Rho (its current API doesn't offer it to us), and stored
  Rho transaction sync for reconciliation matching.
