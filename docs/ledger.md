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

## The catalog and vendor links (`ledger.sync`, `ledger.link_vendor`)

Every post is checked against Payvy's copy of the books before anything is
sent, so a closed month or a wrong account is caught as a named refusal on
the bill. `ledger.sync` (Routine; an agent may run it) reads the books into
`ledger_records`: accounts (type, active, required segment types),
periods (and a closed span before the books' start date), vendors
(archived, default expense account, and their id in another system) and
segments. Nothing is sent to the ledger. A row that leaves the books is
marked inactive, never deleted. Settings → Ledger runs it after connecting
and on **Sync now**.

The checks it feeds, in `payvy-ledger`: a bill line's account must be in
the catalog, active, and an expense account; it must carry a segment of
each type the account requires while the books enforce segmentation; a
segment must be active and take postings; the bill date must not fall in a
closed or locked period or before the books start. A line with no account
takes the vendor's default expense account from the books, then the
company's default.

A bill posts only for a **linked** vendor (`vendor_unlinked` otherwise).
`ledger.vendor_matches` suggests each vendor's ledger vendor: the same id
in another system, matched by source and id together (VisiBooks writes
Acme Ops' vendor 9 as `acme_vendor_9` with source `acme`; an id with no
source never matches), else a unique same name. A person links it
(`ledger.link_vendor`, person-only Admin); the ledger vendor must be
synced, not archived and not linked to another vendor. Once one of the
vendor's bills is posted the link can't change or be removed: its
payments must post to the same ledger vendor as the bill.

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
retries with backoff (attempts² minutes, capped at an hour). A claimed
job carries a two-minute lease: if the worker dies mid-post the job is
reclaimed when the lease expires, which the post's idempotency key
makes safe. After eight failed attempts the job is `dead` — it stops
retrying, its last error stays on the job and on the bill's link, and a
person retries by hand. A bill whose approval-mode post is queued
before its pay step (still approving, or held) defers without spending
an attempt and posts when it gets there. Only a hold defers: a bill
still being approved or waiting on a signal ends its job, and the
approval or signal that finishes the workflow queues a fresh one (a
rejected or voided bill's job ends too — there is nothing to post).
A failure that says nothing about the job — the ledger unreachable, a
call timed out — never counts toward the eight: an outage of any
length retries with the same capped backoff and posts when the ledger
returns, where a refusal (closed period, unmapped account) goes dead
at eight for a person to sort out. No row lock is held across a ledger
call, including the document-link follow-up after a post. The manual
commands remain the by-hand path and run at once: `ledger.post_bill`,
`ledger.post_payment`, `ledger.void`.

## What is not posted yet

- The catalog is read when someone syncs, not on a schedule; sync again
  after adding vendors or accounts in VisiBooks. Confirming bank-feed
  clearing for voids of settled payments is a later phase of the Allied
  cutover. See `docs/visibooks-adapter.md` for the wire contract and
  the Allied brief for the rollout gates (statement reconciliation
  before any live Allied payment).
