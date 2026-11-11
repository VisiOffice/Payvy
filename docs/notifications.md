# Approval and payment-ready notifications

An admin enables company email notices in **Settings → Notifications**. They are
off by default, including after upgrade. Enabling them includes existing open
bills that are currently actionable; it does not mail old completed steps.
The notification worker starts with web sign-in (`PAYVY_PUBLIC_URL`) and does not
require a bank connection. It checks about once a minute and uses the same
Cloudflare/SES mail adapter as sign-in. No new provider credentials are needed.

## Who receives mail

- A current person with Approver or Admin, eligible for the bill's current approval
  step under its pinned workflow and separation-of-duties rules.
- A current person with Payment releaser or Admin, eligible for the current manual
  release step. An automatic step produces no notice unless it falls back to a
  person (for example, over the auto-pay limit).
- Named lists and `require: all` matter. People who already approved that step, who
  entered/uploaded a bill when prohibited, or who must be saved for a later step
  are excluded. No notice is sent for holds, signals, or completed/closed bills.
- API-key-only identities have no sign-in email and are not recipients. Roles are
  checked again immediately before claiming a send, as are active membership,
  recipient email, the step, and company settings. No arbitrary email recipient,
  subject, or body can be supplied through the notification commands.

Email identifies the company, vendor, invoice, amount, due date (if known), and a
link to the bill. It includes no attachment, bank details, sign-in credential, or
approve/pay link. A signed-out reader signs in, then returns to that bill; only
validated local bill routes may survive sign-in. The app still authorizes actions.
If the email has access to multiple companies, select that company's sign-in link.

## Reminders

The company can turn approval reminders off independently. When enabled, the
first reminder becomes due 48 hours after the initial notice was accepted by the
sender, then every 24 hours after the preceding notice, at most three reminders
per person per step. This is waiting time, not the invoice due date. There are no
payment reminders or escalation recipients in this version. A delayed worker
sends at most one due reminder per pass and starts the next interval at the send;
it never sends accumulated reminders together.

Removing eligibility, moving the bill forward, or turning reminders off cancels
queued mail that is no longer actionable. Turning all notices off pauses pending
mail; re-enabling checks its relevance before sending. A cancelled notice is not
resurrected. A person who is removed and later added has a new membership and can
receive a fresh notice. There are no per-person preferences in this version.

## Delivery and history

The bill's **Email notifications** section shows up to the latest 500 notices and
their recipients, step, reminder number, status, and safe failure reason. Refresh
reads the latest state without sending anything. Queuing, sending, settlement,
and settings changes also append events.

Each `(company, bill, step, person, reminder number)` is unique. Multiple workers
serialize briefly on the bill/notice while queuing and claiming; provider HTTP
holds no bill, company, or role lock. A durable `sending` checkpoint is committed
before HTTP. Preview rolls back database effects and never sends mail.

- **Sent to provider:** accepted by the sender, not proof of inbox delivery. No
  bounce/complaint webhooks or open tracking are implemented.
- **Logged only:** development sender; no email left the server. Configuring a
  real sender later does not replay these notices.
- **Queued:** waiting or rate limited. Explicit HTTP 429 responses retry after
  2, 4, 8, and 16 minutes; five total attempts, then failed.
- **Failed:** explicit refusal/bounce, or rate-limit attempts exhausted. No
  automatic resend or subsequent reminders for this notice.
- **Delivery uncertain:** timeout, ambiguous response, server error, redirect, or
  a `sending` record left unfinished for five minutes. It is never replayed and
  blocks subsequent reminders for that recipient/step. This deliberately trades
  possible missing mail for avoiding duplicate non-idempotent sends. The inbox
  remains the authoritative action queue; review the sender's logs if necessary.
- **No longer needed:** cancelled before sending after a fresh eligibility check.

There is no exactly-once guarantee from the email providers. A crash after the
checkpoint but before HTTP can leave an unsent notice marked uncertain. A role,
workflow, or settings change after the checkpoint cannot retract mail in flight;
the linked app always checks current access. Provider response bodies and
credentials never enter notification history or events. No manual resend button
is provided in this first version.

## Commands and storage

- `notification.settings`: company read access.
- `notification.configure`: admin person only.
- `notification.list {bill_id}`: company read access.
- `notification.queue {bill_id}` / `notification.deliver {notification_id}`:
  admins and automation, constrained by company opt-in and evaluated eligibility.
  Calling them early, repeatedly, or for another company cannot generate extra
  notices. No-change attempts return conflict.

Migration `0018_notifications.sql` adds RLS-protected settings and a durable outbox.
No bill status is stored or changed. The worker uses keyset pagination across all
open bills (no newest-N cutoff), handles time-based workflow transitions, and
sends at most 50 notices per company per pass with five concurrent provider calls.
A large backlog or a slow sender can take multiple passes. Deploy the backend and
migration before the frontend, verify the sender, then enable notices per company.
