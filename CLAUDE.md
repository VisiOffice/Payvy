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
- **The README compares Payvy with Bill.com.** Keep every Bill.com figure sourced and dated, and
  keep the "what Bill.com has that Payvy doesn't" list honest.
- SQL is static strings (`concat!` with the column macros); sqlx 0.9 refuses runtime-built SQL.

## Running

```bash
docker compose up -d                      # Postgres on :5446, SeaweedFS (S3) on :8336
cp .env.example .env                      # set PAYVY_API_KEY
set -a; . ./.env; set +a
cargo test                                # integration tests need PAYVY_TEST_* from .env
cargo run -- serve                        # http://127.0.0.1:7430, MCP at /mcp
cargo run -- key create --actor robert --kind person
```

Before committing: `cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check`.
