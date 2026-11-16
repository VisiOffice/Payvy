# Work order: card-free trial, first-run screen and welcome email for new companies

Written 2026-11-13 against VisiOffice/Payvy main `014e8c5` and VisiOffice/payvy-hosted main `83eed50`. Both are live in production.

Robert reviews through a separate reviewer agent and merges. You don't merge, push to main or deploy. Three pieces, in this order: the trial (part 5, payvy-hosted plus its small core PR), the first-run screen (part 4, core), the email (parts 1 to 3, core plus payvy-hosted). Separate PRs, each saying what it does and how it was tested. Stop and report when CI is green.

## The problem

A person who signs up on payvy.com gets the same email as someone asking for a sign-in link: subject "Sign in to Payvy", three lines of plain text (`sign_in_mail`, `crates/payvy-server/src/auth.rs:189`). It is the first thing a new customer receives from Payvy and it doesn't name their company, say what Payvy is for, or say what to do next.

## What to build

### 1. HTML mail with a plain-text alternative (core)

`Mail` (`crates/payvy-server/src/mailer.rs:11`) carries only `text`. Add an optional `html`. When it is set, the SES and Cloudflare backends send both parts (SES v2 `Content.Simple.Body` takes `Text` and `Html` together); the log and memory backends keep both. When it is not set, nothing changes for existing mail.

Rules for every HTML mail:

- The text part is always present and complete on its own, with the full link.
- No remote images, no tracking pixels, no link rewriting, no web fonts. The Payvy name is styled text.
- Inline styles only, one column, readable at phone width and in dark mode.
- Every value that comes from a person (company name, person name, the note in part 3) is HTML-escaped in the HTML part. Add a test with `<script>` and `&` in the company name.
- The sign-in link appears as a button and again as plain text under it, so it can be copied.

### 2. A welcome email, separate from the sign-in email (core)

`operator.rs:279` sends the link for a newly created company through `auth::email_link`, the same function `/auth/link` uses. Give that call its own message:

- Subject: `Welcome to Payvy: {company} is ready`.
- Body, in this order:
  1. One line: the company was created, by name, for this address.
  2. The sign-in button. Under it: the link works once, for 15 minutes; if it has expired, ask for a new one at `{public_url}/sign-in`.
  3. "What to do first", short steps in the same order as the first-run screen in part 4, each one a thing the product does today. With part 5 a new hosted company can work from the first minute, so choosing a plan is not the first step. Check each step against the app before writing it; leave out any that isn't true for a new company.
  4. The optional note from part 3.
  5. One closing line with the public site link and a reply-to or help address if one is configured. No unsubscribe footer: this is a transactional message.
- The sentence "If you didn't ask for it, ignore this email: nobody can sign in without opening it" stays.

The ordinary sign-in email keeps its subject and wording and gains the same plain HTML frame (name, button, link, the same two sentences). Nothing promotional goes in it.

The core is open source and self-hosted by others, so the welcome text describes the product only. It must not mention prices, trials, plans or payvy.com unless that comes from configuration (`PAYVY_PUBLIC_URL`) or from the note in part 3.

### 3. A note from the management app (core and payvy-hosted)

Trial length and plan terms belong to the private app, not the core. Add an optional `welcome_note` to the operator's company-creation input: plain text, at most 500 characters, no links rendered from it, escaped in the HTML part, placed as its own paragraph. Refuse a longer value with a stable error code.

In payvy-hosted, `PayvyCore#create_org` (`app/services/payvy_core.rb:34`) passes a note built from the app's own trial setting, for example: "Your 14-day trial has started, no card needed. Add a plan in Settings > Billing before it ends." The number comes from the same constant the plan policy uses, not a second copy.

### 4. A first-run screen instead of a bare Billing page (core web)

Today the sign-in link drops a new hosted company straight onto Settings > Billing (`Home` in `web/src/App.tsx` redirects a company with no plan). The first thing a new customer sees is two prices and no sign that the signup worked.

Replace that landing with a first-run screen for a company that has no plan yet:

- A heading that names the company and says it was created.
- A short checklist, in this order, each row showing done or not done from real state, not from a stored "seen" flag:
  1. Send the first bill: show the company's own inbound address with a copy button, and the upload button.
  2. Connect the bank that pays.
  3. Invite the people who approve.
  4. Choose a plan. The two plan cards sit here, the same component the Billing page uses, with the terms text from `billing.get`. Not required during the trial; the row says how many days are left.
- Above the list on a hosted install: "Trial: N days left. No card needed until then." from the trial end in `billing.get`.
- Payvy's own reading is the one thing the trial doesn't include. Wherever the app offers it, show it as locked with one line: "Included in Payvy reads for you ($99 a month)", linking to the plan cards. Don't let it fail after a click.
- After Checkout returns, land back on this screen with step 4 ticked, not on Billing.
- `Home` goes to this screen until the person dismisses it or every step is done; it stays reachable from the menu until then.
- When the trial has ended with no plan, `Home` goes to Billing as it does today for a company with no plan, and the page says the trial ended on that date.
- Self-hosted installs (hosted mode off) get the same screen without step 4 and without the trial line.

Only people who can run `billing.checkout` see the plan cards; anyone else sees "Ask an admin to choose a plan".

Tests (browser): a new company in its trial lands on the first-run screen and can upload a bill; choosing a plan and returning ticks step 4; a company whose trial ended with no plan lands on Billing; a member without the admin role doesn't see the plan buttons; a company with a plan and all steps done never sees the screen.

### 5. The trial starts at signup, with no card (payvy-hosted, small core change)

Decided by Robert on 2026-11-13: a new company gets 14 days without giving a card. During the trial it has everything in Bring your own agent. Payvy's own reading is never part of a trial: it turns on only when the $99 plan has been paid for.

Today the trial is Stripe's (`trial_period_days` in `StripeCheckout#checkout`), so nothing works until a card is entered. Move the trial into the private app:

- **Account:** add `trial_ends_at`. Set it at signup to 14 days after creation (`Plans::TRIAL_DAYS`). Backfill existing accounts with creation time plus 14 days.
- **At signup:** once the core has created the company, push the trial entitlement (reader off, payments on) in the same request. If the push fails the signup still succeeds and the sync job retries; until it lands the core shows the company as having no plan, which is correct.
- **`PlanPolicy.evaluate`** takes the account's trial end as well as the subscriptions:
  - A live subscription decides, as now, with one change: `managed` grants the reader only when the subscription is `active` or `past_due` within grace. `managed` in `trialing` grants what `byoa` grants. This is the rule that keeps the reader behind a payment whatever Stripe or the portal allows.
  - No live subscription and the trial hasn't ended: reader off, payments on, reason "trial until {time}".
  - No live subscription and the trial has ended: lapse, as now.
- **Checkout:**
  - `byoa` chosen during the trial: the subscription's `trial_end` is the account's `trial_ends_at`, so the first charge is the day the trial ends. Stripe needs that at least 48 hours ahead; with less than that left, start the subscription now.
  - `managed`: no trial, charged at Checkout, reader on when the payment succeeds.
  - After the trial has ended: no trial on either plan.
  - `trial_used` and `trial_days` in the offers follow from this; remove what is no longer needed rather than keeping two notions of a trial.
- **Expiry:** the hourly `ResyncAccountsJob` must re-evaluate accounts whose trial end has passed since the last run, so payments switch off within the hour without any Stripe event. Test it with the clock.
- **One trial per address:** a second company created by an address that has already had a trial starts with the trial already ended. The signup still succeeds.
- **`/api/billing/status`** returns `trial_ends_at` and whether the trial is running. The core keeps only known fields from that answer, so add the field there (`crates/payvy-server/src/billing.rs`) and to `billing.get`'s output. The Billing page's notice changes with it: in trial, "Trial until {date}. Add a plan to keep paying bills after that."; ended, "Your trial ended on {date}. Choose a plan to send payments again."
- **Plan text** (`Plans::CATALOG`, the offers): Bring your own agent reads "Free for 14 days, then $39 a month" during the trial; Payvy reads for you reads "$99 a month, charged today".

Tests (payvy-hosted): the policy table above, one case per row; signup pushes the trial entitlement; expiry by the resync job; `managed` in `trialing` never grants the reader; Checkout parameters for each of the cases; the second-company rule.

Not in this order: reminder emails before the trial ends. The private app has no mail sender yet. Say in the report what it would take.

## Tests

For parts 1 to 3:


- The welcome mail is sent for a newly created company and names it; a repeated creation call (same idempotency key) sends nothing.
- `/auth/link` still sends the sign-in mail, not the welcome mail.
- Both parts are present in what each backend sends; the text part contains the full link.
- Escaping, as above.
- `welcome_note` over the limit is refused; absent, the paragraph is absent.
- The existing link rules are unchanged: one use, 15 minutes, the per-address limit.

## Don't change

- The link lifetime, the token format, or the per-address limit.
- What `org.create` returns for `sign_in_link`.
- Any secret, key or production setting. Nothing here is deployed by you.

## In the report

Paste the rendered text part and a screenshot of the HTML part for both emails (the memory backend's output opened in a browser is enough), in light and dark.

## Also

### The hosted app's 512 MB memory cap kills one-off Rails commands

Not part of this order; recorded here so it isn't lost.

payvy-hosted `config/deploy.yml` caps the web container at `memory: 512m` (under `servers.web.options`, next to `cpus: 1` and `pids-limit: 256`). A one-off command run with `kamal app exec --reuse` starts a second Rails process inside that same running container, so the web server and the one-off share the 512 MB.

Seen on 2026-11-15 at 03:11 UTC, against the deployed `83eed50`: `kamal app exec --reuse "bin/rails runner '...'"` with a small read-only script (list the accounts and their subscriptions, tally the Stripe events) was killed before it printed anything. Kamal's full error:

```
ERROR (SSHKit::Runner::ExecuteError): Exception while executing on web: Exception while executing on host <web host>: docker exit status: 137
docker stdout: Nothing written
docker stderr: Nothing written
```

Exit 137 is SIGKILL. With nothing on stdout or stderr and a hard memory limit on the container, the kernel's out-of-memory killer is the likely cause; the host's `dmesg` was not checked to confirm it. The web process itself stayed up and `/up` kept returning 200.

`kamal console` was not tried. It boots Rails the same way in the same container, so expect the same result.

What worked instead: the same question asked in SQL through the database accessory, which doesn't boot Rails: `kamal accessory exec db --reuse "psql -U payvy_hosted payvy_hosted_production -c '...'"`.

To fix, either raise the web container's limit so a second Rails process fits, or run one-offs in their own container (`kamal app exec` without `--reuse`), which gets its own 512 MB. The second was not tested.
