---
type: spec
project: Payvy
status: proposed
updated: 2026-11-10
tags: [payvy, ramp, payments, spec]
---

# Ramp Integration Spec

Add Ramp as Payvy's second paying provider. Payvy remains responsible for invoice
intake, vendor matching, approvals and release; the customer's Ramp account executes
the payment. The connector belongs in the open-source product. Hosted Payvy operates
the shared OAuth application; self-hosted installations supply their own application
credentials and callback configuration.

**Proposed first release:** USD ACH payments to existing Ramp vendors, released by
a person in Payvy, with authenticated status reconciliation. Prefer Ramp's dedicated
Payments API, subject to a sandbox and access milestone. Keep automatic payment,
vendor creation, bank-detail collection, checks and bill synchronization for later.

This is a design proposal, not an implemented connector. Code baseline: Payvy main
at `3cc048b`, including Mercury recipients, auto-pay and webhooks, and Rho reads.

## API choice

Ramp offers two materially different paths:

| Path | Consequence for Payvy | Decision |
| --- | --- | --- |
| Dedicated Payments API | Can create vendor payments without a Ramp bill; create requires `X-Idempotency-Key`. Marked beta. | Preferred v1, gated on access and verified behavior. |
| Bill Pay API | Directly created bills are auto-approved; draft submission supports Ramp review. Bill creation does not document an idempotency key. | Separate fallback spec if Payments API access is unavailable. |

The dedicated API exposes ACH and check instruments. It accepts an external
reference and supports looking up payments by that reference. Do not create a Ramp
bill and independently create a payment for the same Payvy release. Those are
alternative execution paths, not two steps of one payment.

Sources: [Payments API](https://docs.ramp.com/developer-api/v1/api/payments),
[OpenAPI schema](https://docs.ramp.com/openapi/developer-api.json),
[Bill Pay](https://docs.ramp.com/developer-api/v1/bill-pay),
[bill payment guide](https://docs.ramp.com/developer-api/v1/bill-payments).

## First milestone and release gates

Use a Ramp sandbox business and application to establish the actual contract before
building the payment UI. No production payments are part of this spec exercise.

1. Confirm Payvy can receive Payments API beta access, required scopes and production
   approval. Ramp's partner process requires review before production access; a
   marketplace listing is a separate step. Include the requested security materials
   in launch planning. [Partner guide](https://docs.ramp.com/developer-api/v1/build-with-ramp)
2. Prove an authorized entity and funding account can pay an existing verified ACH
   instrument. Confirm how account eligibility is represented on the payment path;
   do not assume that every listed bank account can fund payments.
3. Establish whether direct payment creation executes without additional Ramp
   approval, and which Ramp policies can block it. Do not promise Mercury-style
   request mode: the payment schema has no corresponding approval state.
4. Verify same-key/same-body replay, concurrent replay, conflicting-body handling,
   idempotency retention, and external-reference lookup after an interrupted create.
5. Confirm completion, failure, return/reversal and cancellation semantics, supported
   late-return observation window, webhook coverage, scheduling cutoff behavior and fees.
6. Confirm whether standalone payments create accounting artifacts or exports in Ramp.
   Define one owner for ledger posting before connecting Payvy accounting sync, to
   prevent duplicate ledger entries.

If beta access is unavailable, finish connect/read/propose support and leave payment
release disabled. A bill-based fallback needs its own reviewed approval, duplicate
prevention and recovery design; never silently switch endpoints at runtime.

## User experience

**Settings → Bank.** Show “Connect Ramp” alongside Mercury. A person signs into Ramp,
authorizes access, chooses the business entity and funding account, then confirms
activation. Show the business name, entity, masked funding account, connection
health and update method. Do not display an invented balance when Ramp supplies
no usable balance. A connected-but-ineligible account says what setup is missing.

Retain one active paying connection per Payvy organization. Connecting Ramp must
not silently replace Mercury. The confirmation explains the replacement and that
vendor payees must be linked again. Rho remains an independent read-only connection.
Keep old attempts associated with their original connection and credentials for
reconciliation; switching providers must never resend them through Ramp.

**Vendor → Payment details.** Search existing Ramp vendors, select an eligible ACH
instrument and propose the mapping. Show the vendor, bank label and last four.
Only a person can accept it. A Ramp vendor ID alone is insufficient when the vendor
has multiple accounts. Never silently use a changed default account. Missing or
changed details require setup in Ramp and a new proposal in Payvy. Hide the Mercury
invite and typed-recipient forms for Ramp in v1.

**Bill → Release.** Preserve the existing preview and single-use confirmation.
Preview names Ramp, the entity, source, vendor, masked destination, USD amount,
initiation date and any disclosed fee. Use “Send through Ramp,” not “Approve in Ramp”
or “Arrives today.” Confirmation authorizes the specific payment snapshot. Changes
to the amount, payee, source, date or fee require a fresh preview.

**After release.** Show scheduled/processing/completed or actionable failure details.
Authentication loss or uncertainty says “Payment status needs checking” and blocks
another payment. A safe provider link is shown only if supported and verified;
do not fabricate dashboard URLs. No API cancel/retry button in v1: direct the person
to Ramp and reconcile the result before permitting a fresh release.

## Authentication and deployment

Use Authorization Code OAuth for hosted multi-company access. Ramp supports client
credentials for an organization's own integration; offer that as a self-hosted
configuration, with server-side token renewal. Sandbox and production use separate
apps. The access token's `expires_in` controls renewal. Third-party refresh responses
currently do not replace the refresh token, so preserve the original when absent.
[Authorization](https://docs.ramp.com/developer-api/v1/authorization)

Proposed scopes: `business:read`, `entities:read`, `bank_accounts:read`,
`vendors:read`, `payments:read`, `payments:write`. Verify these against the enabled
application and each endpoint. Do not request bill, accounting, card, approval or
vendor-write access for this first release.

Add a person-only `bank.authorize` command that creates an expiring, single-use
OAuth state bound to the person, organization, environment and expected callback.
An OAuth callback is transport for a narrowly validated command completion, not a
new unrestricted write API. It stages credentials; a person-only, confirmed
`bank.connect` activates the selected entity and source. The callback must not
replace the live paying connection automatically. Agents cannot connect accounts.

Keep client secrets, access tokens, refresh tokens and webhook secrets sealed on the
Rust backend, bound to organization, connection and secret purpose. Never place
them in Vite variables, command results, URLs emitted to logs, tracing or events.
Use exact configured redirect URIs, one-use state, strict environment checks and
PKCE if supported by the registered app. Prevent concurrent token renewals with a
connection lock; refresh failure disables new sends without erasing pending attempts.

Use a callback under `/auth/ramp/…`, already covered by the frontend Worker proxy.
Register webhooks on the public Rust API origin under `/webhooks/ramp/…`.
Disconnect stops new sends immediately. Retain monitoring access until outstanding
payments and the supported return window are resolved, then revoke credentials;
if access is revoked externally, preserve the unresolved state for a person.

## Commands and provider boundary

Reuse `bank.accounts`, `bank.recipients`, `vendor.propose_payee`,
`vendor.accept_payee`, `bill.release`, `payment.list` and `payment.refresh`.
Extend their typed inputs/results where necessary; regenerate web command types.
Every state change and worker action remains in the command registry with an event
in the same transaction. All new tables enforce organization RLS.

The current implementation calls `bank::mercury` directly from payment submission
and refresh, and assumes every account has a balance and every recipient has only
method/display information. Introduce explicit provider dispatch and capabilities
for accounts, payee discovery, submission, observation and webhook management.
Do not force Rho to implement a payment method, or interpret Mercury's `request`
mode as a generic guarantee of bank approval.

Add typed Ramp configuration and credential records. Persist business/entity/source
IDs on the connection; vendor/instrument IDs and a destination fingerprint on the
accepted payee. Re-read the instrument at preview and commit. Account removal,
ineligibility or fingerprint change blocks sending and requires another acceptance.
Only masked bank details leave the connector, even if a provider response contains
full numbers. Treat unknown balances as unknown, not zero.

## Sending once and recovering uncertainty

For each release, freeze the provider request: source/entity/vendor/instrument IDs,
integer USD cents, initiation date, invoice number and external reference. Use the
persisted Payvy attempt UUID as both the basis of `X-Idempotency-Key` and
`external_ref`. Keep the exact request stable across midnight, worker retries,
token refresh and process restarts.

Commit the attempt, frozen request, release authority, consumed confirmation and
event **before** making the external POST. A worker claims the durable attempt and
sends it. Reuse existing confirmation semantics while adding this durable submission
boundary; rolling back an ordinary database transaction cannot undo a Ramp payment.
Enforce one live attempt per bill, including unresolved attempts, under concurrency.

Timeouts, lost responses and uncertain server errors stay `submitting`. Reconcile by
payment ID, or paginate an exact external-reference query with
`include_canceled=true`; require matching business context, vendor, instrument,
source, amount and currency before associating a result. Multiple or mismatched
results require investigation. No result is not proof that nothing was sent.

Only replay the same request/key inside the contract verified in the first milestone.
After that window, or without a proven retention guarantee, stop automatic POSTs
and require reconciliation. Never use a fresh key to recover an uncertain send.
Rate-limit responses back off according to provider guidance; authentication or
permission failures must not accidentally clear the unresolved-payment lock.

## Status and reconciliation

Proposed mapping from the dedicated Payment object's status:

| Ramp | Payvy | Bill behavior |
| --- | --- | --- |
| `SCHEDULED`, `PROCESSING` | `pending`, retaining provider detail | Released; no second payment. |
| `COMPLETED` | `sent` | Paid after authenticated observation and identity/amount checks. |
| `CANCELED` | `cancelled` | Person can release again after confirmed cancellation. |
| `ERROR` before completion | `failed` only for a verified terminal failure | Hand back to a person. |
| Verified return after completion | `returned` | Reopen; require person release. |
| Unknown or contradictory status | Nonterminal reconciliation hold | Never assume failure or permit repayment. |

Ramp also exposes ACH detail and failure reasons. A timeout or new enum must not be
mapped using Mercury's default-to-failed logic. If a completed payment later becomes
an error, verify return semantics before reopening it. Preserve raw status codes and
safe diagnostic IDs; sanitize provider error text. The existing 14-day Mercury
post-send polling window must become provider-specific, with Ramp's window decided
in the first milestone. Late signed updates still trigger a fetch after routine
polling ends. [API schema](https://docs.ramp.com/openapi/developer-api.json)

Start with the existing minute-based polling worker, adding backoff and per-connection
limits. Add `payments.updated` webhooks for prompt updates while keeping polling as
recovery. Ramp signs raw request bytes with HMAC-SHA256 in `X-Ramp-Signature`; its
subscription verification includes a challenge and an API verification step.
[Webhooks](https://docs.ramp.com/developer-api/v1/webhooks)

Store the subscription secret before activation and handle verification separately.
Verify signatures in constant time and bind `business_id` to the connection. Durably
deduplicate event IDs before acknowledging; enqueue a `payment.refresh` command,
then fetch Ramp's current state. Do not mark a bill paid from the webhook payload.
Do not transplant Mercury's timestamp/signature format. If durable receipt fails,
return a retryable error. A webhook setup failure leaves polling active and visible.

## Scope and implementation sequence

1. **Access and contract spike.** Sandbox fixtures for eligibility, payment creation,
   idempotency, scheduling, return states and webhook verification. Record launch
   prerequisites and endpoint/schema versions. No money-moving production calls.
2. **Connection and payees.** Provider dispatch, sealed OAuth lifecycle, capability
   reporting, account selection, existing Ramp vendor/instrument proposal and human
   acceptance. Preserve Mercury and Rho behavior.
3. **Manual payment pilot.** Durable submission, provider-specific observations,
   polling, release preview and recovery UI. Feature-gated to selected organizations.
4. **Push updates and launch.** Verified webhooks, operational visibility, documentation,
   hosted app approval and self-hosted setup guide. Remove the pilot gate only after
   end-to-end reconciliation and access prerequisites pass.

Follow-ups: ACH auto-pay through the same submission path and existing accepted
workflow authority; checks with explicit address selection and fee review; Ramp
vendor creation/onboarding; a separate bill-export/approval workflow if customers
need Ramp-side review. No automatic payment in v1, no automatic retry after a
definite failure, no card/wire/international support, no multi-bank routing and no
implicit bank-detail sync. Make automatic-workflow bills require person release
when Ramp lacks auto-pay capability, rather than leaving them silently stuck.

## Acceptance criteria

- Fake Ramp proves connect → account selection → propose/accept payee → preview →
  confirm → one payment → authenticated completion → bill paid.
- Preview sends nothing; an agent cannot authorize a connection, accept a payee or
  manually release. Existing auto-pay cannot accidentally run through Ramp.
- Crash before send, crash after remote acceptance, dropped response and failed
  local completion all recover one attempt with the same body/key. Concurrent
  workers and a second release cannot produce a second payment.
- Changed instruments, replaced connections, stale confirmations, mismatched amounts,
  unknown states and expired idempotency windows block unsafe submission.
- Token expiry, refresh races, revoked access, rate limiting and missing scopes have
  actionable UI states. Cross-organization OAuth callbacks, payee IDs, event deliveries
  and payment IDs cannot cross RLS or connection boundaries.
- Duplicate, forged, delayed and out-of-order webhooks cannot pay/reopen a bill by
  themselves. Polling recovers missed notifications; confirmed late returns require
  human release. Replayed OAuth state and verification challenges are rejected.
- No complete bank numbers, credentials or raw provider payloads appear in persisted
  events, errors or telemetry. Integer-cent and currency checks run before sends.
- Browser tests cover hosted OAuth handoff, multiple funding accounts/instruments,
  preview/confirm, uncertain status, reconnect and switching from Mercury.
- Existing Mercury, Rho, confirmation and payment tests remain green; run repository
  clippy/fmt/type checks and applicable browser tests before merging implementation.

## Decisions remaining before production

The recommended product scope is settled above. External dependencies remain:
Payments API beta availability for Payvy, required partner review, direct-payment
approval behavior, idempotency retention, source eligibility/fees, and late-return
semantics. Resolve them in the access spike. If direct payments cannot satisfy a
customer's required Ramp approval policy, keep their payment release disabled and
design the draft-bill handoff explicitly rather than bypassing their policy.
