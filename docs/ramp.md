# Ramp integration development

Ramp supports staged OAuth authorization,
confirmed connection activation, and explicit vendor/ACH-account mapping through
the command API and web app. It has **no payment-create, cancel, retry, vendor-write
or bill-write method**. Activating Ramp selects it in place of the paying bank,
but sending and auto-pay remain disabled. Rho remains independent.

See [the implementation spec](ramp-integration-spec.md) for the full sequence and
production gates. Ramp's dedicated Payments API is beta; local fake tests do not
verify account access, provider idempotency retention or production behavior.

## Implemented

- Fixed production and sandbox API origins, no redirects, bounded responses and
  complete pagination restricted to the original origin/resource. Broken or
  repeated pages fail instead of returning a misleading partial inventory.
- Business, entities, funding accounts, vendors and ACH instrument discovery.
  Typed results drop unknown fields and reject malformed last-four values.
  Funding accounts have no synthetic balance; each instrument has its own ID.
- Payment reads and exact-reference lookup, including cancelled payments. Unknown
  or contradictory states need review; `ERROR` does not automatically unlock a
  payment retry. Results are informational until the command layer matches them
  to the complete persisted attempt and its business context.
- OAuth client-credentials and refresh transports. Refresh preserves an omitted
  refresh token; its original expiry deadline must be retained by the caller.
  Token types and clients deliberately do not implement Debug or Serialize.
- Errors expose categories and HTTP status only, never raw provider bodies or
  transport URLs. No implicit HTTP retries. Rate limits retain a numeric retry
  delay where supplied; callers must apply a bounded fallback otherwise.

## Verification

```bash
cargo test -p payvy-pay
```

These tests use an in-process loopback HTTP fake, with no bank credentials or real
network calls. They cover discovery, redaction, unsafe pagination/redirects,
incomplete inventories, reference recovery, conservative states and token refresh.

For the first real sandbox discovery check, supply either
`PAYVY_RAMP_SANDBOX_TOKEN`, or `PAYVY_RAMP_SANDBOX_CLIENT_ID` and
`PAYVY_RAMP_SANDBOX_CLIENT_SECRET` through your local secret manager/environment.
Do not commit them. The application needs these read scopes:

```text
business:read entities:read bank_accounts:read vendors:read payments:read
```

Then run:

```bash
cargo run -p payvy-pay --example ramp_discovery
```

The example always calls `https://demo-api.ramp.com`; it cannot select production
or send a payment. It prints business/entity/account discovery and a vendor count,
never credentials. It has not yet been exercised against an authorized Ramp sandbox.

## Configure and try setup

On the Rust server, set these through your secret manager/environment (not in
Vite or the Cloudflare frontend):

```text
PAYVY_SECRETS_KEY=<existing 64-character hex encryption key>
PAYVY_RAMP_CLIENT_ID=<Ramp app client ID>
PAYVY_RAMP_CLIENT_SECRET=<Ramp app secret>
PAYVY_RAMP_ENVIRONMENT=sandbox
PAYVY_RAMP_REDIRECT_URI=https://your-payvy.example/settings/bank
```

Register the exact redirect URI in the matching Ramp app and enable the
Authorization Code and Refresh Token grants. Only the discovery scopes above
are requested. Sandbox and production use separate apps. The client ID and
environment are bound to each authorization/connection; changing them requires
reconnecting. A debug-only `PAYVY_RAMP_SANDBOX_URL=http://127.0.0.1:<port>` exists
for the local fake. Release builds refuse it.

In **Bank → Ramp**, authorize, choose an entity and funding account, then review
and confirm. The callback only stages sealed credentials. A ten-minute state is
bound to the company and initiating person; expired/reused states and another
person's callback are refused. A failed exchange consumes its state and requires
a fresh authorization. No automatic code retry. Reloading a ready screen retains
only its stage ID in session storage. OAuth query parameters are removed from the
visible URL after return; provider errors/bodies and tokens never enter events.

Activation verifies the current business, USD entity, ACH account and entity
account membership, and compares their fingerprint with the confirmed input.
The preview names the previous connection and exact masked account. A changed
account or previous connection requires another review. The old Mercury webhook
is removed on a best-effort basis, matching Mercury replacement; previews do not remove it and cleanup failure does not block
activation. The old connection's sealed credentials remain so polling continues
to monitor existing Mercury attempts; existing payees do not carry over to Ramp. Rho is not retired. The database represents
Ramp as `setup`, reserves the selected-paying-bank slot, and rejects unsupported
Ramp modes. `bill.release` refuses a Ramp send; `pay_elsewhere` remains available.

On a vendor page, use **Search Ramp vendors**, select **Vendor in Ramp**, then a
specific **Ramp ACH account**. Search is sent to Ramp with the name/active filters;
selecting or accepting an account fetches only that vendor and its instruments.
The proposal records the connection, remote vendor ID, instrument ID, last four,
and a fingerprint including the displayed name and fee. Only a person can accept
it in **Waiting on you**. Acceptance reads Ramp again and refuses changed,
missing or inactive destinations. No vendor or account is created in Ramp.
Vendor pages warn when an accepted mapping belongs to a previous connection.
Waiting on you labels stale proposals and disables Accept; the command still
checks the current connection even if the page is stale.

Commands: `bank.authorize`, `bank.complete_authorization`, `bank.authorization`,
`bank.activate` (person-only, confirmed), `bank.maintain`, `bank.renew`, and the existing
`bank.recipients`, `vendor.propose_payee`, `vendor.accept_payee`, `bank.disconnect`.
Activation requires an explicit `replacing_connection_id`, including `null` if
none. Ramp proposals require `connection_id`, `instrument_id` and
`instrument_fingerprint` from discovery, as well as `recipient_id` and `ach`.

## Automatic renewal and connection health

The server runs a Ramp checker independently of Mercury payment polling. Every
30 seconds it looks for due live connections (up to 50 per pass) and runs
`bank.maintain` as the bank agent. All writes and health events still go through
the command registry. Maintenance can check an existing grant and renew its
access token; it cannot authorize, activate, change an account, accept a payee,
or send a payment. Already-retired grants are skipped; an in-flight response is
discarded if the connection was retired or changed meanwhile.

The checker renews within five minutes of access expiry, otherwise checks the
business every fifteen minutes. Requests are separated by at least one minute,
including short-lived grants; a very short grant may expire before the next
scheduled check. A per-grant advisory lock serializes workers and manual checks.
Provider calls hold neither the company bank lock nor the bank connection row lock,
so payee decisions, disconnects and replacements can proceed while Ramp is slow.
After HTTP, a short bank lock protects rechecking the original live connection and
credential snapshot before saving; late responses cannot rewrite a retired grant.
The original refresh token and its absolute expiry are retained. A
successful refresh is sealed and committed even when the following business
probe fails. Previews send no health probes or refresh requests.

`bank.get` exposes the last recorded health and check time, access expiry, retry
time and available recovery action, never credentials or provider error bodies:

- **Connected:** access was checked; renewal is automatic when the grant includes
  refresh access. A successful business probe must match the selected business.
- **Waiting to retry:** transient failures use 1, 2, 4, 8 and 16 minute delays.
  Rate limits use the numeric Retry-After, bounded to 1 minute–24 hours, with a
  one-minute fallback. The retry time is persisted, so restarting the server,
  another worker, or a person's check cannot bypass it.
- **Checks paused:** five consecutive failures stop automatic attempts. A person
  can use **Check Ramp connection** (`bank.renew`) after cooldown to start a new
  bounded recovery cycle. Agents cannot resume a paused cycle.
- **Needs reconnection:** rejected or expired authorization, missing refresh
  access when renewal is due, unreadable credentials, a changed server app, or
  a mismatched/inactive business stops renewal. A person must fix the underlying
  configuration where necessary, authorize again, and confirm replacement.

Health describes the last scheduled business/access check, not every Ramp
endpoint's current availability. A vendor endpoint can fail between checks; the
mapping screen shows the error and links to Bank settings. Bank settings refresh
health every thirty seconds while visible and on focus. Current mappings remain
subject to fresh destination validation when proposed or accepted. Ramp payment
submission and auto-pay are still disabled regardless of connection health.

## Verification of activation and mapping

```bash
# Set the local PAYVY_TEST_ADMIN_URL / PAYVY_TEST_APP_URL from .env.example.
cargo test -p payvy-server --test ramp --test payments --test rho
pnpm --dir web build
cargo build -p payvy-server
PAYVY_E2E_BANK_ONLY=true pnpm --dir web e2e:only ramp.spec.ts ramp-health.spec.ts bank.spec.ts payee-invite.spec.ts recipient.spec.ts
```

The server tests use real Postgres RLS and a loopback fake, covering person/org
binding, expiry/replay, failed OAuth exchange, replacement previews, changed
funding, instrument identity, changed/inactive payees, pre-expiry renewal across
concurrent workers, cooldown persistence, retry exhaustion, rejected grants,
renewal deadlines, stale mapping flags, and the send gate. Browser tests drive the real API and only fake Ramp's consent
redirect and bank endpoints; they exercise callback, selection, confirmation,
proposal, person acceptance, stale-mapping warnings, and automatic-renewal
failure/reconnection on desktop and mobile. The health browser test uses the
real worker and a short fake grant, so it takes about two minutes. These tests do not verify
real Ramp app access or production eligibility.

## Next slice

Exercise the whole authorization/activation flow with an authorized Ramp sandbox
and confirm entity funding-account eligibility for the beta Payments API. Then,
after the external access/contract gates are satisfied, add durable pre-send
attempt storage and person-confirmed ACH submission. Keep auto-pay disabled for
Ramp until that path and late-return reconciliation are proven.

Sources checked 2026-11-10:
[OpenAPI](https://docs.ramp.com/openapi/developer-api.json),
[authorization](https://docs.ramp.com/developer-api/v1/authorization),
[sandbox](https://docs.ramp.com/developer-api/v1/sandbox).
