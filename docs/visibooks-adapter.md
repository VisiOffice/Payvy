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
- Missing due dates export as the invoice date; Payvy's source bill stays unchanged.
- Payment methods map `domestic_wire` to `wire`, and `real_time_payment`/`other`
  to `other`; ACH, check, wire, and card retain their method. Unknown methods
  are refused rather than guessed.
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

## Phase 1 wiring (this change)

The adapter is now hooked into the app, per company:

- **Settings → Ledger** (`ledger.connect`, person-only Admin, confirmed):
  origin, entity ID and API key. The key is sealed with
  `PAYVY_SECRETS_KEY` under purpose `visibooks` and never read back;
  reconnecting updates the same connection row, so its links stand. The
  origin is stored on the row (migration `0029_ledger_origin`).
  `ledger.settings` reads the connection without the credential;
  `ledger.configure` (Admin) changes `post_when`, the AP account, coding
  defaults and the bank account map, refusing a no-change request.
- **Adapter per connection**: a VisiBooks connection builds
  `VisiBooks::new(origin, entity, unsealed key)` at post time; other
  providers keep the server-configured adapter (tests use the fake).
- **post_when**: at `approval`, `bill.approve` posts the bill when its
  last approval lands; at `payment`, `payments::apply` (bank sent) posts
  the bill first, then the payment. Failures are recorded on the bill's
  ledger link by the post itself (`record_failure`) and never roll back
  the approval or the bank facts. `bill.mark_paid` in payment mode posts
  the bill and emits `ledger.payment_skipped` for the payment leg: an
  outside payment has no Payvy payment row and no bank account to map.
- **After posting**: the bill's document link is set
  (`PUT …/bills/by_external/payvy/{key}/document_link`) to the bill's page
  in the app; when the bank reference is learned — including arriving
  with no status change — `PUT …/vendor_payments/by_external/payvy/{key}/bank_reference`
  follows. Neither is part of a post body or its hash.
- **Service period**: a bill's first allocated PO period rides on
  `BillBody.service_period` (and therefore its request hash) and is sent
  as `service_period: {starts_on, ends_on}`.
- VisiAPI-side contract notes for Phase 0: the two follow-up endpoints
  above accept `{document_link}` / `{bank_reference}` JSON bodies and any
  2xx JSON answer; the entity header and bearer auth are as in #45.

## Still required before cutover

- Merge/review PR #41 and the VisiAPI recording API.
- Reference catalog reads and cache refresh; linking existing contacts without
  replacing Quarry's source identifiers; handling contact merges.
- Confirmation of bank-feed clearing. A payment already
  settled in VisiBooks currently requires reconciliation before its ledger void.
- Signed evidence links in the core body model; attachment API (the
  document link above is a URL in Payvy, not an uploaded file).
- Existing-bill import links, AP ownership/intake/workflow/funding gates.
- Reviewed statement reconciliation, real account coding confirmation, and
  normal human payment release. Consolidated ACH and card funding remain separate.

No production ownership, bank connection, credentials, or payments changed while
building this slice. The reviewed manual reconciliation remains the live-payment gate.
