# CLAUDE.md

Payvy: open-source, agent-first accounts payable. Rust workspace (web app to come).
Scope and decisions: `~/Documents/Obsidian Vault/Projects/Payvy/Payvy Rebuild Brief.md`.
Conventions follow Visirent (`~/Code/visirent`).

## Rules that hold the design together

- **Payvy never moves money.** It instructs the customer's own bank (Mercury, Ramp, Brex)
  or records a payment made elsewhere. Never add a rail that holds or transmits funds.
- **Every action is a command** in `payvy-domain/src/commands.rs` (`COMMANDS`), dispatched in
  `payvy-server/src/commands/mod.rs`. Never add a REST route or UI path that writes outside it:
  MCP tools are generated from the registry, so a bypass is invisible to agents.
- **People sign off, agents prepare.** `person_only` commands (approve, reject, release, void,
  accept/decline a workflow) refuse agent keys, and agents don't see them in `tools/list`.
  Don't clear `person_only` to make an agent flow easier.
- **Confirmed tier** (release, mark paid, void): preview → single-use token bound to the input →
  commit. A preview runs the real command and rolls back.
- **A resent invoice stacks, never pays twice.** Every copy of an invoice (vendor +
  normalized number, as `bill.create` detects) files with its bill in `bill_arrivals`
  and emits `bill.invoice_received`; same amount with the same lines is a recorded
  resend, a different amount — or the same total with different quantities on both
  sides' quantity-bearing lines (`bills::quantities_differ`) — on an open
  bill is a pending revision that blocks `bill.release`/`bill.auto_pay`
  until a person accepts (`bill.revision_accept`: the bill takes the copy's amount and
  lines) or rejects it. `Sent N×`/`Revised` show on the bill, in lists and in Waiting
  on you. `allow_duplicate` remains the recorded override for a separate bill. See
  `docs/duplicate-invoices.md`.
- **Where a bill stands is derived, never stored.** `workflow::evaluate` over approvals, signals
  and the release gives the status every time. Commands check the evaluated status before acting.
  Don't add a `status` column to shortcut this.
- **Bills pin their workflow version.** Workflow rows are never edited, only moved
  proposed → active → superseded (or declined). Editing a vendor's workflow never changes a bill
  already in flight.
- **Writes return the new state and append an event in the same transaction** (`Cx::emit`).
  A write that changes nothing is an error, never a bare success.
- **Money is integer cents.**
- **Row-level security**: every query runs through `Store::begin(org_id)`. New tables get
  `org_id`, `ENABLE` + `FORCE ROW LEVEL SECURITY` and the `org_isolation` policy. The dev
  database role is not a superuser, so RLS is really enforced.
- **The clock** is read only through `AppState::clock` (`Cx::now`), so tests can move time.
- `payvy-domain` stays pure: no I/O, no clock.
- **Documents live in S3-compatible storage** (`payvy-storage`: rusty-s3 signing + reqwest, no
  SDK), keyed by content hash per organization. The type comes from the bytes
  (`documents::check`), never the filename or a claimed content type. Effects outside the
  database are skipped when `cx.preview`; the object is written before the row, so a failed
  transaction leaves only an unreferenced object.
- **Email arrives only through the signed webhook** (`/webhooks/email`, `inbound.rs`), which
  runs the `email.import` command as the agent `email`. Parsing is in `payvy-domain/src/email.rs`;
  the Cloudflare worker (`workers/email-inbound`) only signs and forwards raw bytes. Keep it that
  way so any mail front end works. The worker and server share a signature test vector
  (`inbound::tests`); change both or neither. Outbound mail (sign-in links, later notifications) goes
  through one adapter (`mailer.rs`) with two backends: Cloudflare Email Service's REST API, the
  default we document for self-hosters (same account as inbound), and Amazon SES (API v2, signed
  by `sigv4.rs`, tested against AWS's vector), which hosted Payvy uses. No AWS SDK.
- **Models read, Payvy decides.** Extraction (`document.extract`, `payvy-extract`) asks the model
  only for what is printed, with amounts as strings. Converting to cents, checking, vendor matching
  and duplicates live in `payvy-domain/src/extraction.rs`, which is pure and tested, so a weaker
  model produces drafts that fail checks, never wrong bills. The same schema goes to both backends
  (Claude Messages API, OpenAI-compatible chat completions); don't add a provider-specific schema.
  An extraction never creates a bill: `bill.create` with `extraction_id` does, and records what was
  corrected. When the prompt or schema changes, run `payvy extract-eval fixtures/invoices`.
- **Payvy never moves money; the customer's bank does** (`payvy-pay`, `commands/bank.rs`,
  `commands/payments.rs`). Store the bank's recipient id and the last four digits, never an
  account or routing number. Bank tokens are sealed with `PAYVY_SECRETS_KEY` (`secrets.rs`) and
  never appear in a result or event. Payees are proposed by anyone and accepted only by a person.
  Every attempt has a deterministic idempotency key; an unclear answer (`BankError::Unclear`)
  leaves the payment `submitting` to be resent with the same key, never a new one. Only the
  bank's answer marks a released bill paid (`payments::apply`). New banks implement the same
  shape in `payvy-pay`; tests use a fake bank (`tests/payments.rs`), never a real one.
- **Mercury invitations are non-idempotent.** `vendor.invite_payee` is person-only
  and confirmed. It commits an unknown attempt, its event and the consumed token
  before calling Mercury; a crash or unclear response must never automatically
  resend. This is an intentional two-transaction command. Its preview skips the
  checkpoint and the POST. Completion only proposes a payee; the payment worker checks open invitations every five minutes (`check_invites`), and only a person recovers one by its Mercury ID. Invite error bodies
  and onboarding links must never reach events or logs. See `docs/payments.md`.
- **Auto-pay is pre-authorization, not a bypass.** `bill.auto_pay` (the payment worker runs it as
  agent `workflow`) pays only `ready_to_pay` bills, only through Mercury, only to an accepted payee,
  via the same `payments::submit`. A payment that doesn't go sets `bills.release_by_person`, which
  turns the auto pay step into `awaiting_release` (`workflow::Facts`); auto-pay never retries.
  Above the auto-pay limit (`auto_pay_limits`: the company's, default $5,000, never unlimited;
  a vendor's own overrides it either way) the step is `awaiting_release` with a reason naming
  which limit applied, and `payments::submit` checks the limit again under its advisory lock
  before sending; every limit change takes that lock. Anything that raises the limit in force for
  some bill is confirmed (`auto_pay.raise_limit`), the rest routine; both are person-only. Don't
  add a way around it for automatic payments.
- **Typed bank details pass through.** `vendor.create_recipient` (person-only, confirmed) sends
  account and routing numbers to Mercury and keeps only the recipient id and last four; never put
  them in a result, event, error or log (`payvy_pay::redact_numbers`). It never retries a POST.
- **Mercury webhooks are nudges, not truth.** `bank.connect` registers one (`PAYVY_WEBHOOK_URL`,
  production only, never fails the connection); `mercury_webhook.rs` verifies `Mercury-Signature`
  and then runs `payment.refresh`, which asks Mercury. Never take a payment's state from the payload.
- **Vendor/company cases are enumerated policies, never core code** (`vendor_po_policies`,
  `po.policy_get`/`po.policy_set`, person-only Admin). Each change is a new immutable
  version; bills pin the version at creation (`bills.policy_version_id`), so a change
  never rewrites a bill in flight, and no pin reads the defaults (flag; do not silently
  match). First policy: `over_period` (`flag_only` default; `require_full_cover` makes
  out-of-period allocations not count as covering). A partly covered bill is
  `over_available` when no active PO has room for the rest, else `uncovered`; caps are
  unchanged. A PO can be suspended (`po.suspend`/`po.reactivate`, person-only Admin):
  allocations stand, new ones refused, its bills carry `suspended_po` until reactivated
  or overridden. Agents may not set policy. See `docs/purchase-orders.md`.
- **An overridden bill never auto-pays.** A bill payable only because of overrides
  (`Facts::payable_by_override`, computed from `bill_exceptions` + valid overrides)
  evaluates to `awaiting_release` with a reason, never `ready_to_pay`, at any
  workflow's automatic pay step — so the payment worker never picks it up and no
  retry loop exists. Someone other than the overrider releases it (`bill.release`).
- **Quantity matching is cumulative, exact, and policy-gated** (`payvy-domain/src/quantity.rs`).
  Bill lines carry quantity/unit/unit-price as printed (extraction details, invoice review,
  or `bill.create`); a bill line maps to a PO line by normalized description (+unit when
  both state one) — unmatched/ambiguous are findings, never silent mappings. Quantities are
  exact micro-units, rates exact to the cent: over-ordered is `quantity_over`, a cent off
  the PO rate is `rate_mismatch`. The registry's `quantity_matching` policy (pinned like
  the rest) is `flag_only` by default and `enforce` to block payment. Amount-only lines
  are never quantity-checked. See `docs/purchase-orders.md`.
- **Rho is a read-only connection** (`payvy-pay`'s `Rho`, mode `read_only`): it coexists with
  the paying bank (one live paying connection per org, plus one read-only connection per
  provider), its client has no pay method, and `bill.release` with only Rho behaves exactly
  as with no paying bank — paid elsewhere, then `bill.mark_paid`. Never route a payment,
  recipient or payee through it; tests use a fake Rho (`tests/rho.rs`), never a real one.
- **Ramp is setup-only** (`mode: setup`): no payment submission or auto-pay. Its
  independent checker runs `bank.maintain` for an already-authorized grant.
  Renewals serialize per grant, then recheck under the bank lock after HTTP. Never
  hold the company bank lock or connection row lock during maintenance HTTP. Discard
  late results for changed/retired connections. Retain the original refresh expiry,
  and persist health/backoff even when the provider fails. Five transient failures
  pause automatic attempts; only person-only `bank.renew` resumes after cooldown.
  Rejected grants require reconnecting. Never clear a person-only gate to renew,
  and never put credentials or provider response bodies in health, events or logs.
- **The web app (`web/`) is one more caller of the commands.** Vite, React 19 and Astryx
  (`@astryxdesign/*`, exact versions pinned; it is in beta). It calls `/api/commands/{name}` with
  the session cookie and the `x-payvy-client` header, which the server requires for cookie
  sign-in so a cross-site form can't use the cookie. Inputs are typed from the registry:
  after changing a command's schema, run `pnpm --dir web gen:api` and commit
  `web/src/api/commands.{json,gen.ts}`. Nothing in the app may write around a command, and
  anything an agent can't do in the API, the app can't do for an agent either. Use Astryx
  components before writing CSS; `pnpm --dir web astryx component <Name> --dense` shows one.
  The workflow builder (`web/src/workflows`) lives on each vendor's page (`/vendors/:id`) for its
  own workflow and on `/workflows` for the default, which also lists the vendors that differ. It
  edits the same steps the server
  evaluates and saves through `workflow.propose` then `workflow.accept`, so a person's edit is
  on the record like an agent's. Its checks (`problems` in `web/src/workflow.ts`) mirror
  `Workflow::validate` for quick feedback; change both together, and the server still decides.
  Workflows are one line of steps, so the chart is CSS, not a graph library.
- **Company roles authorize every person command** (registry `permission`, `actor_roles`).
  Admin is full access, not a workflow bypass. AP clerk, Approver, and Payment releaser
  combine. Read roles on every invocation, including confirmed commits; never trust UI
  capabilities or an actor name as authorization. Membership/role changes take the roles
  lock before checking the caller, protect the last sign-in admin, and emit old/new roles.
  Removing someone revokes their person keys as well as sessions. Agent person-only gates
  remain unchanged. See `docs/roles.md` for migration and operator recovery.
- **Workflow emails are a durable outbox, not approvals** (`notification.*`, `notification_worker`).
  Company opt-in, current roles, pinned workflow, and duties determine recipients; recheck before
  claiming. Commit `sending` before non-idempotent provider HTTP, release all business locks,
  and never replay unknown delivery. Only explicit rate limits get bounded retries. Preview
  never sends. Initial notices and capped approval reminders are unique by bill/step/person/
  ordinal. Sender response bodies must never enter history or events. See `docs/notifications.md`.
- **People sign in with a link sent by email** (`auth.rs`, `people` table). No passwords. The
  token travels after `#` and is posted by the app, so mail scanners can't use it up; only
  hashes of link and session tokens are stored; asking for a link answers the same for unknown
  addresses. A person signs in as the actor of the same name, with a person key's rights.
  Outbound mail goes through `mailer.rs` (Cloudflare Email Service, or the log in development).
- **The README compares Payvy with Bill.com.** Keep every Bill.com figure sourced and dated, and
  keep the "what Bill.com has that Payvy doesn't" list honest.
- SQL is static strings (`concat!` with the column macros); sqlx 0.9 refuses runtime-built SQL.

## Running

```bash
docker compose up -d                      # Postgres on :5446, SeaweedFS (S3) on :8336
echo 's3.bucket.create -name payvy-dev' | docker compose exec -T s3 weed shell  # once; then
                                          # docker compose restart s3 (SeaweedFS takes uploads to a
                                          # missing bucket and then 404s every download)
cp .env.example .env                      # set PAYVY_API_KEY
set -a; . ./.env; set +a
cargo test                                # integration tests need PAYVY_TEST_* from .env
cargo run -- serve                        # http://127.0.0.1:7430, MCP at /mcp
cargo run -- person add --email you@example.com --name You --actor robert --roles admin
cargo run -- key create --actor robert --kind person        # optional API access for that person
pnpm install                              # once: the web app and the email worker
pnpm web                                  # the app on http://127.0.0.1:5173; the sign-in link
                                          # is printed in the server's log
pnpm e2e                                  # builds both, then the browser tests (web/e2e)
```

Before committing: `cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`,
and `pnpm -r check` when `web/` or `workers/` changed.

### End-to-end tests (`web/e2e`)

Playwright drives the built app on the real server (`target/debug/payvy`, port 7431) against a
fresh database it drops afterwards, and a `payvy-e2e` bucket in the compose SeaweedFS. Nothing
else is mocked except the model and the banks: `fake-model.ts` answers as an OpenAI-compatible API
with what each fixture invoice prints, so extraction, checks and vendor matching run for real;
`fake-bank.ts` stands in for Mercury and Rho, reached only through sandbox connections
(`PAYVY_MERCURY_SANDBOX_URL`, `PAYVY_RHO_SANDBOX_URL`, accepted only by debug builds and only on
`http://127.0.0.1`; production connections always go to the real bank). Sign-in
links are read from the server log (no mailer configured), so the tests sign in as a person
does. Setup data is made through the command API with an agent key; what only a person may do
(accept, approve, release, reject) is done in the browser. Run them when a change touches the
web app or a command it uses. First time: `pnpm --dir web exec playwright install chromium`.
`pnpm --dir web e2e:only` reruns without rebuilding.

The bank tests (`bank.spec.ts`, `payee-invite.spec.ts`) need no documents:
`PAYVY_E2E_BANK_ONLY=true pnpm --dir web e2e:only payee-invite.spec.ts bank.spec.ts` runs them
without S3; the full suite still needs the document store. It creates its own
database like the other browser tests. Never use production bank credentials.
