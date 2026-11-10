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

- Paying automatically when a workflow's `pay` step is `auto` (today a person releases).
- Inviting a vendor to enter their own bank details in Mercury.
- Bills in other currencies, and Wise and Airwallex for international payments.
- Ramp and Brex.
- Paying through Rho (its current API doesn't offer it to us), and stored
  Rho transaction sync for reconciliation matching.
