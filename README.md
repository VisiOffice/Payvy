# Payvy

Open-source, agent-first accounts payable.

Payvy takes in bills, moves each one through its vendor's workflow (approvals, holds, waiting on
outside events) and pays through **your own** bank: Mercury, Ramp or Brex. It never holds your
money, charges no per-payment fees and has no float. Self-host it or use hosted Payvy.

**Status:** early. The workflow engine, bills, vendors, audit log, REST API and MCP server work.
Bill intake (email, upload, extraction), bank payments, accounting sync and the web app are next.

## Vendor workflows

Every vendor's bills follow a workflow: an ordered list of steps ending in payment.

| Step | What it does |
|---|---|
| `approve` | Named people (or anyone) sign off; one of them or all. Can apply only from an amount. |
| `hold` | Wait a number of days after the previous step. |
| `wait_for` | Wait for a signal from outside, e.g. `customer_funded` when your customer has paid. |
| `pay` | Pay automatically, or wait for a person to release it. |

Agents can propose workflows; a person accepts them. Bills keep the workflow version they
arrived under. Each workflow comes with a Mermaid chart.

## Agents

Every action is a command, served the same way over REST (`POST /api/commands/{name}`) and MCP
(`POST /mcp`). People's keys can approve and release; agents' keys can't, and don't see those tools.
Payment-related commands preview first and commit with a single-use token.

## Running it

See `CLAUDE.md`. Requires Postgres.

## License

AGPL-3.0-only.
