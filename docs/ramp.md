# Ramp integration development

Branch: `feat/ramp-integration`. The first slice is a connector library and local
fake-provider tests. It is not yet selectable in Settings or `bank.connect`, and
has no payment-create, cancel, retry, vendor-write or bill-write method. Mercury
and Rho continue through their existing paths.

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

## Next slice

Implement command-owned OAuth state and callback completion, encrypted credential
storage with serialized renewal, and staged connection activation. Add entity and
funding-account selection and provider-aware payee mapping without enabling sends.
The current server's `bank.connect` input still accepts Mercury/Rho only; do not
paste a Ramp token into its Mercury token field.

After the external access/contract gates are satisfied, add durable pre-send
attempt storage and person-confirmed ACH submission. Keep auto-pay disabled for
Ramp until that path and late-return reconciliation are proven.

Sources checked 2026-11-10:
[OpenAPI](https://docs.ramp.com/openapi/developer-api.json),
[authorization](https://docs.ramp.com/developer-api/v1/authorization),
[sandbox](https://docs.ramp.com/developer-api/v1/sandbox).
