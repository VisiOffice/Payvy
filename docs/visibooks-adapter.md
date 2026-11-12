# VisiBooks ledger adapter — initial transport

Branch `feat/visibooks-ledger`, based on PR #41 (`feat/ledger`) and current main.
This is the first transport slice of the Allied cutover brief, not a deployable
connection or an authorization to pay Allied.

`payvy_ledger::VisiBooks::new(origin, entity_id, api_key)` implements `Ledger`:

- Posts bills and records already-sent payments through the matching VisiAPI
  branch `feat/payvy-allied-api` (`docs/payvy-ledger-api.md`).
- Uses the existing deterministic bill/payment keys. The core keeps its existing
  canonical request hash; VisiAPI independently hashes the translated financial
  body under a unique entity/kind/key receipt. No cross-service hash equality is
  assumed. Retries keep the same key and body.
- Converts decimal strings to integer cents without floating point; checks
  overflow and refuses zero/negative lines until VisiBooks supports them.
- Explicitly checks the configured entity against the body; sends the entity
  header and a sensitive bearer header. HTTPS only in the public constructor,
  no redirects, 30-second timeout, maximum 64 KiB response. Provider errors are
  mapped to fixed safe messages, not copied into bill history.
- Returns VisiBooks numeric record IDs and uses entity-scoped receipt-backed void
  aliases. Repeated voids succeed; an already-voided post is a conflict.
- Current native VisiBooks bill lines carry one segment. At most one configured
  department/class/location dimension can be sent; it must contain a VisiBooks
  segment ID. Multiple dimensions are refused, never silently dropped.
- Current payments contain one bill application. A multi-application ledger API
  does not mean Payvy can send consolidated bank payments.
- File attachment explicitly returns unsupported; it never reports success
  without uploading anything.

## Validation

Normal tests use an in-process HTTP fake and do not contact external services:

```sh
cargo test -p payvy-ledger
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

A separate ignored test exercises the real Rails API over loopback with disposable
fixtures. It posts/replays, refuses a changed body, records/replays a payment,
refuses bill void while payment is active, then voids both twice.

1. In the VisiAPI branch, prepare an isolated **test** PostgreSQL database using
   `RAILS_ENV=test bin/rails db:schema:load` and migrations as needed. Set
   `DATABASE_URL` explicitly to that disposable database for every Rails command.
2. Run `RAILS_ENV=test PAYVY_CONTRACT_FIXTURE=/tmp/payvy-contract.json bin/rails runner script/payvy_contract_fixture.rb`.
   The fixture script refuses non-test environments and writes a new mode-0600
   file containing only disposable credentials. It refuses to overwrite a file.
3. Run `RAILS_ENV=test bin/rails server -b 127.0.0.1 -p 17543` against that same database.
4. In Payvy run:

   ```sh
   PAYVY_VISIBOOKS_TEST_FIXTURE=/tmp/payvy-contract.json cargo test -p payvy-ledger local_visiapi_contract -- --ignored
   ```

5. Stop the test server and delete the disposable fixture/database when finished.
   Never use production API keys or bank credentials for this test.

## Still required before cutover

- Merge/review PR #41 and the VisiAPI recording API.
- Reference catalog reads and cache refresh; linking existing contacts without
  replacing Quarry's source identifiers; handling contact merges.
- Per-org credential sealing/connection factory and Settings → Ledger UI. This
  constructor is not wired into production `AppState` yet.
- Automatic posting on approval/payment, the `post_bills_at` policy, late bank
  reference updates and confirmation of bank-feed clearing. A payment already
  settled in VisiBooks currently requires reconciliation before its ledger void.
- Signed evidence links/service periods in the core body model; attachment API.
- Existing-bill import links, AP ownership/intake/workflow/funding gates.
- Reviewed statement reconciliation, real account coding confirmation, and
  normal human payment release. Consolidated ACH and card funding remain separate.

No production ownership, bank connection, credentials, or payments changed while
building this slice. The reviewed manual reconciliation remains the live-payment gate.
