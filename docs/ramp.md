# Ramp integration development

Branch: `feat/ramp-integration`. Ramp now supports staged OAuth authorization,
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

On a vendor page, select **Vendor in Ramp**, then a specific **Ramp ACH account**.
The proposal records the connection, remote vendor ID, instrument ID, last four,
and a fingerprint including the displayed name and fee. Only a person can accept
it in **Waiting on you**. Acceptance reads Ramp again and refuses changed,
missing or inactive destinations. No vendor or account is created in Ramp.

Commands: `bank.authorize`, `bank.complete_authorization`, `bank.authorization`,
`bank.activate` (person-only, confirmed), `bank.renew`, and the existing
`bank.recipients`, `vendor.propose_payee`, `vendor.accept_payee`, `bank.disconnect`.
Activation requires an explicit `replacing_connection_id`, including `null` if
none. Ramp proposals require `connection_id`, `instrument_id` and
`instrument_fingerprint` from discovery, as well as `recipient_id` and `ach`.

Access renewal is **manual in this slice**: use **Renew Ramp access** in Bank or
`bank.renew`. It serializes against other renewals and connection changes,
reseals the new credentials, and preserves the original refresh-token deadline
when Ramp omits it. Preview never exchanges credentials. Expired/missing/revoked
refresh grants require reconnecting. Background renewal and automatic recovery
from authorization failures are not implemented.

## Verification of activation and mapping

```bash
# Set the local PAYVY_TEST_ADMIN_URL / PAYVY_TEST_APP_URL from .env.example.
cargo test -p payvy-server --test ramp --test payments --test rho
pnpm --dir web build
cargo build -p payvy-server
PAYVY_E2E_BANK_ONLY=true pnpm --dir web e2e:only ramp.spec.ts bank.spec.ts payee-invite.spec.ts recipient.spec.ts
```

The server tests use real Postgres RLS and a loopback fake, covering person/org
binding, expiry/replay, failed OAuth exchange, replacement previews, changed
funding, instrument identity, changed/inactive payees, renewal deadlines and the
send gate. Browser tests drive the real API and only fake Ramp's consent
redirect and bank endpoints; they exercise callback, selection, confirmation,
proposal and person acceptance on desktop and mobile. These tests do not verify
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
