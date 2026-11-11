# Invoice line review — pilot

The pilot keeps a correctable invoice breakdown from extraction through bill creation.
It is off by default. An Admin enables it in Settings → Company → Invoice line review,
using `invoice_review.configure`. Turning it off stops enrollment of new documents;
previously reviewed originals and every split view of those files remain subject to review,
including pages returned by Undo split.
All bills accept negative adjustments and zero rows, including manual and agent-created bills.
A nonempty breakdown must reconcile exactly to a positive bill total, with overflow rejected.
Future accounting exports and line-level PO matching must support signed line amounts.
Only document-backed bill creation takes the company enrollment lock; manual entry does not.

## Review a document

The current reader still extracts description and amount. Review shows those original
rows even before a vendor is matched and even when they do not reconcile. Edit amounts,
add rows, move rows up, or exclude/restore a row with a reason. Discounts are negative;
zero rows are retained. Select whether tax is already included or added once. Missing
amounts do not become zero. The total must reconcile exactly, with no synthetic balancing
line. A matching sum is not evidence that the model read the invoice correctly.

Save review keeps incomplete data and all prior revisions. Check the invoice details,
currency and pages against the original. Saving complete data enables Create bill. A
re-read preserves the review; choosing the newer reading starts an explicit new revision
lineage. The old revisions and model output stay available. Clicking Source line
searches the PDF text; this is a viewing aid, not verified field-level grounding.

If a breakdown cannot be established, an authorized person with the Prepare permission
may explicitly accept summary-only after saving and checking the headers. A reason is
required. The original rows stay in history; the bill has no posting lines and shows the
summary reason. Agents cannot accept this exception. It neither approves the bill nor
bypasses any workflow, duties, payment or PO gate. A change invalidates the exception.

This first pilot reviews one whole document at a time. Split a multi-invoice file first;
it does not create a reviewed bill from a subset of the document's pages. Saved reviews
retain the page-arrangement version they checked. Before approval, page edits, splits and
Undo update the bill's page arrangement and append the existing `bill.pages_arranged` event.
The saved review and bill lines stay unchanged. Bill detail exposes
`invoice_review_pages_changed` and warns the approver that pages changed after line review;
the page-change history explains what changed and who did it. Restoring the original order
still leaves a record that pages changed. Pages lock at first approval (or release/closure),
following the Document Pages Spec. Review history stays available on filed documents.

## Command contract

- `invoice_review.settings`: Read permission, reports whether new reviews are enabled.
- `invoice_review.configure`: person-only Admin, changes the pilot setting with an event.
- `document.review_get`: Read permission; returns current review, history, initial extraction
  values, arrangement version and deterministic validation. Reading creates no review.
- `document.review_save`: Prepare permission, people or agents. Supply `expected_version`,
  `arrangement_version`, `extraction_id`, `request_id` and the full payload. Initial version
  is zero. Keep all source rows; exclude rather than silently delete. Every write creates
  an immutable revision and event. No-op changes are refused.
- `document.review_preview`: Prepare permission; validates the same input without a write.
- `document.review_accept_summary`: person-only Prepare, scoped to the current saved revision,
  original arrangement and total. Reason required. Creates a new revision recording the choice.
- `bill.create`: supply the current `review_id` and matching headers. Server derives the lines
  and checks the saved revision in the same transaction that files the document. Omitting IDs
  or a line array cannot bypass review. Clients may omit `lines`; supplied lines must match.

The new commands use the shared registry, so REST, MCP and the UI have the same behavior.
Write request IDs are scoped to company/actor, with command, actor kind and canonical input
checked on replay. An identical retry returns the existing revision without another event.
A changed request conflicts. Authorization is rechecked before replay. On an uncertain save,
clients can also reload the current review instead of blindly repeating a mutation.

Rows retain stable IDs and source indices within an extraction lineage. Reviews bind to
one extraction, document and arrangement. The migration adds composite company foreign keys,
forced RLS, and a unique bill-to-review association. Existing bill lines are not rewritten.
The bill total remains strictly positive; all bill lines may be negative or zero and a
nonempty breakdown must sum exactly to the total. Arithmetic overflow is rejected. Existing payment code sends the whole bill amount; this introduces neither partial payment nor vendor credit application.

## Verification and remaining spec work

Domain tests cover discounts, zeros, unreadable values, totals, tax selection and source checks.
Database tests cover immutable revisions, replay, concurrent edits, omission bypasses, summary
scope/person-only restrictions, settings-off behavior, company isolation and stale pages.
Browser tests exercise editing, reload, re-read preservation and actual bill line persistence.
Full-row expectations now accompany the existing synthetic model-evaluation fixtures. No model
prompt/schema changes were made in this pilot; fake-model tests are not accuracy benchmarks.

The broader vault spec is **not complete**. Remaining work includes quantity/unit/rate and
per-line tax extraction, structured source spans with runtime OCR/grounding, model/prompt
version metadata, detailed field-level validation and metrics, improved re-read comparisons,
and the human-labeled real-invoice evaluation corpus (50 invoices / 10 layouts, held-out data).
No production accuracy claim or broad rollout is justified by this implementation alone.

There is no line-data-required workflow setting on the current main branch. When such a policy
is added, summary-only acceptance and bill creation must recheck it. The PO feature is included
from main; an integration test verifies that a person-accepted
summary-only bill still cannot be released when a required PO is missing.

Migration 0022 follows the merged PO migrations 0020/0021. Verify the number against any
newly queued migrations before landing.


Validation on 2026-11-11, with main `436b640` included:
- All 245 Rust tests passed with PostgreSQL and document storage configured.
- Clippy, formatting, workspace type checks and the web build passed. Command inputs are unchanged.
- All 8 focused browser scenarios passed for line review, page editing and splitting. They
  verify that the page-change warning and history remain visible while bill lines stay unchanged.
- The warning was visually checked above the bill's approval/payment controls. PR CI runs the
  complete suite, including the Cloudflare Worker tests.

This is the first review implementation on `feat/invoice-line-review`, not a deployed pilot
or completion of the extraction/evidence stage of the vault spec.

Review decisions: signed and zero lines are allowed on all bills, including unreviewed
person/agent entry. Manual bills still avoid the review-enrollment lock. Reviewed pages
follow the existing approval-time lock; pre-approval changes appear in bill history and
show a warning without changing the saved review or bill lines.
