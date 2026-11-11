# Payvy

[![CI](https://github.com/VisiOffice/Payvy/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/VisiOffice/Payvy/actions/workflows/ci.yml)

**The open-source Bill.com alternative.** Bills come in, follow the workflow you set for each
vendor, and get paid from **your own** bank account: Mercury, Ramp or Brex. Payvy never touches
your money, so it doesn't charge you to move it.

Self-host it for free, or let us run it for one flat price per company.

> **Status: early.** Working today: vendor workflows, bills, the document inbox on any
> S3-compatible storage, email intake, reading bills with Claude or any OpenAI-compatible model,
> paying through Mercury, the audit log, the REST API and the MCP server, and the start of the web
> app (sign-in by email link, bills). Coming next: approving and releasing in the app, the invoice
> viewer, the workflow builder, sync to VisiBooks, QuickBooks Online and Xero, and Ramp, Brex,
> Wise and Airwallex.

## Why not Bill.com

Bill.com is a payments company that sells AP software. Its business model shapes the product:
it charges per user, charges per payment, and holds your money in transit. Payvy is AP software
that pays through the bank you already have.

| | Bill.com | Payvy |
|---|---|---|
| Price | $49 to $89 per user per month; Enterprise is custom | Free to self-host; hosted is one flat price per company |
| Adding an approver | Another paid seat | Free |
| ACH payment | $0.59 each | Whatever your bank charges (Mercury: $0) |
| Check | $1.99 each | Your bank's rate (Mercury mails checks) |
| Faster payment | 1% (min $9.99) or $11.99 for faster ACH | Your bank's real-time or same-day rails |
| Your money | Debited from your account first, then paid out to your vendor; users report waits of several days | Stays in your account until your bank sends it |
| Your vendors | Nudged to join Bill.com's network; receiving fees on some options | Paid however they like, with no fee from us |
| PO matching | Corporate tier ($89/user) and up | Included |
| Multiple entities | Corporate tier and up | Included |
| Accounting sync | Automatic sync from Team tier; the most common complaint in reviews | Every sync logged per item, every error visible and fixable |
| AI agents | Added to the product | The product is built for them: every action is an API command and an MCP tool |
| Source code | Closed | AGPL-3.0, on GitHub |

Bill.com figures are from [bill.com/pricing](https://www.bill.com/pricing) as of November 2026.
The sync and payment-timing observations come from public G2 and Capterra reviews. If something here is out of date, open an
issue and we'll fix it.

**What Bill.com has that Payvy doesn't (yet):** a network of millions of vendors already set up
to be paid, broad international payouts with currency conversion, 1099 e-filing, and a mobile
app. Through Mercury, Payvy pays by ACH, check, domestic wire and real-time payment today;
international payments are planned through Wise and Airwallex.

## How it works

**Vendor workflows.** Every vendor's bills follow a workflow you can see as a chart:

| Step | What it does |
|---|---|
| `approve` | Named people (or anyone) sign off; one of them or all of them. Can apply only above an amount. |
| `hold` | Wait a number of days after the previous step. |
| `wait_for` | Wait for something outside Payvy, e.g. `customer_funded` once your customer has paid for the job. |
| `pay` | Pay automatically, or wait for a person to release it. |

Agents can propose workflows; a person accepts them. A bill keeps the workflow it arrived under,
so changing a workflow never changes a bill already on its way. One person gives a bill one
approval, so two approval steps always mean two people; a company can also require that whoever
entered a bill isn't the one who approves it ([docs/approvals.md](docs/approvals.md)). Company [roles and permissions](docs/roles.md) control who may prepare invoices, approve, release payments, and administer settings.

**Agents prepare, people sign off.** Every action is a command, served the same way over REST
(`POST /api/commands/{name}`) and MCP (`POST /mcp`). Agent keys can read bills, enter them, file
documents, signal events and propose workflows. Only a person can approve, release a payment or
accept a workflow, and agents don't even see those tools. Anything that moves money previews
first and commits with a single-use token tied to exactly what was previewed.

**Documents on your storage.** Original invoices go to any S3-compatible store: Cloudflare R2,
Backblaze B2, AWS S3, or one you run (Garage, SeaweedFS). A file's type is read from its contents,
and the same file arriving twice is caught.

**Workflow notifications.** Opt-in email notices reach eligible approvers and payment releasers, with capped approval reminders and per-bill delivery history. See [notifications](docs/notifications.md).

**Bills by email.** Each company gets an inbound address; attachments land in the inbox, and
forwarded emails work too. Mail comes in through Cloudflare Email Routing, free on your own
domain, or anything else that can post a signed request. See
[`docs/email-intake.md`](docs/email-intake.md).

**Bills read for you, by the model you choose.** New documents are read automatically: vendor,
invoice number, dates, lines, tax and total. Payvy checks the numbers against each other, matches
the vendor, flags duplicates and keeps a draft that someone enters with one command. Use Claude or
anything with an OpenAI-compatible API: OpenAI, Gemini, Grok, OpenRouter, or a model on your own
server so invoices never leave it. Sample invoices in the repo score any model before you trust it.
See [`docs/extraction.md`](docs/extraction.md).

**Try historical invoices.** The [legacy importer pilot](docs/legacy-invoice-import.md)
exports a small sample from old Django Payvy into Documents for review. It includes
a synthetic sample, dry run, and resumable uploads; it does not create bills or payments.

**Paid from your bank, never twice.** Connect Mercury with an API token and link each vendor to
a recipient you already have there. Payvy never stores account numbers, and only a person can
change where a vendor is paid. A release sends the payment under a key the bank honours, so a
retry can't pay twice; in request mode it also waits for approval in Mercury. Payvy watches the
bank and marks the bill paid when the money has gone, or brings it back if the payment fails or an
ACH is returned. See [`docs/payments.md`](docs/payments.md).

**Every change is on the record.** Each change writes an event in the same database
transaction, and organizations are isolated by Postgres row-level security.

## Running it

Requires Postgres and an S3-compatible store; `docker compose up -d` starts both locally. The
web app in `web/` is built with Vite and served by the same server (one Docker image).
See [`CLAUDE.md`](CLAUDE.md) for setup, configuration and tests.

## License

[AGPL-3.0-only](LICENSE). Part of the [Visi](https://github.com/VisiOffice) suite; works on its
own or alongside [VisiBooks](https://visibooks.app).
