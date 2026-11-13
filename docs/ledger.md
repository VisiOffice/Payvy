# Posting to the ledger

How a bill and its payment get from Payvy into the company's books
(VisiBooks), top to bottom. Payvy never moves money and never owns the
books: it posts what the bank already did, or is about to do.

## Connecting (Settings → Ledger)

An Admin connects VisiBooks per company (`ledger.connect`, confirmed):
the HTTPS origin, the entity id, and an API key. The key is sealed with
`PAYVY_SECRETS_KEY` on the way in and never read back — not in a result,
an event, or a log. The origin is validated before anything is stored
(the adapter's own constructor, plus a refusal of private, link-local
and unspecified hosts; loopback stays available for development), and
private network addresses are never called.

Reconnecting with the same entity and origin rotates the key on the
same connection, so its links stand. A **different entity or origin is a
different set of books**: the old connection is retired and a new one
starts with no links carried over.

`ledger.configure` (Admin, confirmed) sets how it posts: `post_when`,
the AP account, coding defaults, and the bank account map (Payvy bank
connection id → ledger bank account id, or a payment method like `ach`
for bills paid outside Payvy). Map keys and a named AP account are
validated against the company's bank connections and mirrored accounts.

## When things post (`post_when`)

Hooks never call the ledger. They queue a job in the same transaction
as the fact that caused it, and the ledger worker posts afterwards in
its own transaction — a slow or failing ledger can stall or roll back
nothing but itself.

- **approval**: when a bill's last approval lands and it reaches its pay
  step, a `post_bill` job is queued.
- **payment**: when the bank reports a payment sent, `post_bill` then
  `post_payment` jobs are queued; the bill carries the **payment date**
  as its issue date. A bill already posted just replays to its stored id.
- **A bill paid outside Payvy** (`bill.mark_paid`) queues the same two:
  the payment leg is keyed by the bill and drawn on the ledger account
  the bank map names for its method. Unmapped, it fails on its link
  where a person can see it.
- After a bill posts, its **document link** is set to its page in the
  app. When the bank reference is learned — including arriving with no
  status change — it is sent once and recorded on the payment's link;
  polls that change nothing send nothing.

## Failure, retry, and doing it by hand

Every post first decides (purely, in `payvy-ledger`) whether the ledger
already has exactly this body: a replay returns the stored id, a changed
body under the same key is a conflict, and a changed bill voids and
reposts. A failure is written on the bill's ledger link (status, code,
detail — the per-item sync log on the bill page) and on the job, which
retries with backoff (attempts² minutes, capped at an hour). The manual
commands remain the by-hand path and run at once: `ledger.post_bill`,
`ledger.post_payment`, `ledger.void`.

## What is not posted yet

- Reference-catalog reads (accounts, periods, vendor currency) are
  mirrored rows the posts check against; refreshing that mirror from
  VisiBooks and linking existing contacts are later phases of the Allied
  cutover, as is confirming bank-feed clearing for voids of settled
  payments. See `docs/visibooks-adapter.md` for the wire contract and
  the Allied brief for the rollout gates (statement reconciliation
  before any live Allied payment).
