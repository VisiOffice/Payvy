# Richer invoice extraction — opt-in pilot

An Admin first enables invoice line review in Settings → Company, then enables
**Richer invoice reader**. Both are off by default. The same commands are available
through REST and MCP: `invoice_review.configure` accepts `extraction_v2_enabled`;
`document.extract` selects the reader using company settings and document enrollment.
This branch does not enable or deploy the pilot anywhere. A v2 extraction reports
`review_required: true`; extraction alone never reports readiness for bill creation.

## What a reader captures

Description and printed amount remain the posting facts. Optional quantity, unit,
unit price, printed tax amount/rate, item code and service dates stay as strings,
including fractional precision. Each field has a present/absent/unreadable/conflicting
state. Null means unknown or absent, never zero. Raw model output remains immutable.
Extracted PO references identify candidates only; they authorize no spending.

The review screen exposes optional details, source quotes and original page numbers.
A source action opens that page and searches its text layer for the quote. Scanned
images have no PDF text layer to highlight; the cited page still opens. Text located
means the quote and value were found with row context, **not** that semantic assignment
is proven correct. No model confidence percentage is presented as accuracy.

Runtime source checks use `pdftotext` on native PDFs, then `pdftoppm` and English
`tesseract` for image-only pages. The Docker runtime and CI install those utilities.
Self-hosters running the binary directly must install them. Missing tools, unreadable
files, timeouts or missing evidence produce uncertainty requiring a recorded source
resolution. Nothing is silently marked verified. Original text stays in the extraction
record under existing company RLS; it is not logged or sent to a second remote service.
The existing configured extraction provider still receives the invoice.

## Review and validation

- Quantity × unit price uses checked decimal arithmetic (up to 18 digits and six
  decimal places per operand), rounded to USD cents, ties away from zero. Unsupported
  formats and discrepancies require correction or an explanation. No amount is rewritten.
- Printed line tax is metadata. The existing explicit tax treatment controls whether
  header tax is included or added once. There is no tax calculator or inferred GL coding.
- Signed/zero lines and exact reconciliation continue to apply to all bills. Whole-bill
  payments, approvals, roles, separation of duties and PO gates are unchanged.
- A source resolution explains missing evidence, changed/removed source facts or unresolved
  field states. A quantity explanation handles a printed bundle, tax, discount or rounding.
  Changing the row clears carried-forward explanations; preview and save apply the same rule.
- Incomplete page coverage requires a separate explanation after checking every selected
  page. Summary-only remains person-only, reasoned and unavailable until header/source/page
  review is complete. It does not approve a bill or satisfy a PO/payment condition.
- Reviewed metadata is pinned through the bill's immutable `invoice_review` revision.
  Posting `lines` retain the existing description/cents/account shape. Consumers needing
  quantity or source facts read `invoice_review.payload.rows`; excluded rows remain there
  with reasons. Do not assume metadata arrays have the same length as posting lines (tax
  may add a posting line, and summary-only posts none).

## Concurrency and bounds

Enrollment is persisted before external work and inherited by split/undo views of the
same original file. Switching off new enrollment cannot bypass an in-flight or failed
richer review. Previously enrolled files continue using v2 when re-read.

A ten-minute document lease refuses overlapping reads without holding a business lock.
Ordinary command and commit failures roll back before attempting token-checked lease cleanup
in a separate transaction (five-second deadline). A database outage, crash or canceled task
can still leave the lease until expiry; a late response cannot clear a replacement lease or become current.
The reader releases document and company locks before model/OCR calls, then reacquires
and checks current permissions, page arrangement and document status. A late answer for
changed pages is retained as stale history, cannot become the current review seed, and
cannot trigger automatic splitting. Other document actions can proceed during the read.

Read at most 24 selected pages per request. Split multi-invoice documents only at verified
invoice boundaries. Keep a single longer invoice intact for manual review; automatic
page batching is not implemented, and agents must not split an invoice just to fit the limit.
The model is
sent the original with an explicit page selection, not a cropped replacement. Its page
coverage declaration is checked against that selection, but remains a model claim to
review. Maximum 1,000 rows, 30 citations per row, 2,000 characters per quote, 128 KiB source
text per page, 20 seconds per OCR subprocess, 90 seconds total local source work and 4 MiB
provider responses. Incomplete/oversized responses fail safely. Provider error bodies are
not stored in document history. The existing background reader retries failed reads after 15 minutes, stopping after
three failures; this can incur provider costs. No additional multi-model review calls are added; the configured provider
fallback behavior is unchanged.

The production API container is configured for 1 GiB memory and a 128 MiB `/tmp` tmpfs
(which counts against that memory). Source extraction is limited to one job per API
process, including native text extraction; waiting for capacity counts against the
90-second deadline. The slot is acquired before creating scratch files. OCR uses one
OpenMP thread, and each rendered PDF page is removed after OCR so pages do not accumulate.
These limits still require validation against real workloads; multiple API processes each
have their own slot. CI retries required OCR package installation four times before failing.

Extraction analysis records schema/prompt versions and hashes, original SHA-256, selected
pages, arrangement version, timestamps, elapsed time, coverage, source method/text and
location results. Existing extraction records retain provider/model identity. AI
usage and estimated cost are recorded per attempt (`ai_usage_attempts`);
`payvy extract-eval` is excluded, and 1-hour cache writes (2x input), the 1.1x
US-only inference surcharge, and batch discounts are not modelled in the price
table.

## Evaluation and rollout gate

`payvy extract-eval fixtures/invoices` retains the v1 regression suite.
`payvy extract-eval fixtures/invoices-v2` uses the v2 schema for cases declaring
`schema_version: 2`. It scores exact labeled fields, explicit nulls, row count (including
repeats) and page coverage, and separately reports evidence location. Missing rows and
invented values fail the score. Configure the intended model before running either command.

The four v2 fixtures are synthetic: fractional rates with zero rows, a negative discount,
a repeated row across two pages, and a scanned image. They exercise mechanics, not accuracy.
Fake-provider Rust/browser tests establish integration behavior only. No accuracy claim,
vendor-layout learning or production rollout follows from those tests.

Before enabling for operational invoices, create a private corpus of at least 50 invoices
across at least 10 vendor layouts, including scans, mixed taxes, service periods, ambiguous
balances, missing fields, long tables and repeated rows. People label the printed facts,
page coverage and evidence; retain 20 documents as held-out cases not used for prompt tuning.
Keep real PDFs and labels outside Git. Use the JSON format in `fixtures/invoices-v2`, adding
explicit nulls for absent fields. Record model/version, exact per-field results, row
precision/recall, wrong-value and abstention rates, page omissions, reviewer correction
time and cost/latency. This initial evaluator does not calculate all those release metrics;
collect them in the pilot report rather than treating its aggregate field score as sufficient.

Approval remains with people. Start with one test company and a small imported sample;
expand only after reviewing failures. Layout-specific prompts, learned coding, real-corpus
accuracy targets and accounting export are subsequent work, not claims made by this pilot.
