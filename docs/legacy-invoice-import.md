# Legacy invoice importer pilot

Import a small set of old Django Payvy invoice files into the new app's **Documents**
queue for extraction and review. This is a command-line pilot on a separate feature
branch, not an accounting migration or a new hosted integration. It creates no
bills, vendors, payees, approvals, payment records, or bank connections.

The tools live in `scripts/legacy_import/`. The importer and sample generator need
Python 3.10+ on Linux and use only the standard library. The exporter runs with the
**legacy Django project's installed dependencies and configuration**, including its
Postgres and object-storage access. Copy the tool directory to that runtime if needed;
there is no need to modify or deploy the legacy application's code.

## First try: synthetic invoices

Run these from the new Payvy repository. The files are existing synthetic fixtures,
not customer data:

```bash
mkdir -p .legacy-import
python3 scripts/legacy_import/sample.py --output .legacy-import/sample
python3 scripts/legacy_import/import.py .legacy-import/sample/manifest.json
```

The second command defaults to an **offline dry run**: it validates the manifest,
file type, size, and hash without contacting Payvy or creating a journal. Sample
creation refuses an existing directory; reuse its manifest for subsequent runs.

To upload into a local/test Payvy instance with S3-compatible storage configured:

```bash
# Use a key issued for the destination test organization. Keep it out of shell history.
read -rsp 'Test organization API key: ' PAYVY_IMPORT_API_KEY; echo
export PAYVY_IMPORT_API_KEY
python3 scripts/legacy_import/import.py .legacy-import/sample/manifest.json \
  --api http://127.0.0.1:7430 --apply
unset PAYVY_IMPORT_API_KEY
```

Open **Documents** in that instance. When the server's automatic reader is enabled,
the documents are read there. If it is disabled (`PAYVY_EXTRACT_AUTO=false`), either
use the UI to read them or add `--extract` to the importer command. Explicit extraction
uses the server's configured model and can incur model costs. Avoid running explicit
extraction alongside the background reader; the existing reader does not serialize
all concurrent model calls. The importer itself skips successful or previously failed
reads rather than repeatedly spending credits on reruns.

Vendor matching may say a vendor is missing; that is expected in an empty organization.
The importer does not seed vendors from old metadata or create bills automatically.
For testing bill creation later, use an isolated organization without a paying bank
and review its workflows first: `bill.create` attaches the current workflow, which
may include automatic payment.

## Export a real legacy sample

First run an inventory, using the old backend's Python environment. `--company-id`
is the legacy `Company` ID, **not** the new organization ID. Dates filter `Bill.created`
(the old invoice date), inclusive. Without dates, selection is the latest entered bills.

```bash
/path/to/legacy/python scripts/legacy_import/export.py \
  --django-root /path/to/old/payvy/backend \
  --company-id 123 --from 2026-01-01 --to 2026-10-31 --limit 25
```

Inventory reads only the database; `eligible` means a source is structurally usable,
not that the storage object was fetched. To actually copy originals:

```bash
/path/to/legacy/python scripts/legacy_import/export.py \
  --django-root /path/to/old/payvy/backend \
  --company-id 123 --from 2026-01-01 --to 2026-10-31 --limit 25 \
  --apply --output /private/export/payvy-sample
```

The output directory must be new and its parent must exist. Add `--settings` for a
non-default settings module, or use the runtime's `DJANGO_SETTINGS_MODULE`. Repeat
`--bill-id 456` to restrict selection to known bill IDs. Limits are 1–50, default 25;
this caps selected bills, so skips can produce fewer usable files. No paid/unpaid
filter is applied: old status and paid amounts are reference data, not instructions.

All ORM reads run inside a PostgreSQL **read-only, repeatable-read transaction**.
No model saves, signals from saves, payment calls, or legacy status updates are made.
Files come through Django's configured storage API; download URLs and storage
credentials are never exported. The generated bill-summary PDF is never used.

The pilot deliberately skips and reports:

- Incomplete source processing, missing page links, source-company mismatch, deleted pages, or shared/partial
  attachments. An original can contain pages assigned to different bills.
- More than one attachment per bill or conflicting invoice numbers.
- Missing originals, unless exactly one complete page image can be used instead.
- Unsupported/empty files or files over 25 MiB. Failed storage reads are reported
  as `source_file_unavailable`; inventory alone cannot establish availability.

It does not split/merge PDFs or assume that the first page represents an entire bill.
These cases can be handled in a later version once the real inventory shows their frequency.

## Import and resume

Transfer the **whole bundle** (`manifest.json` plus `files/`) to a private directory,
then run the same dry run and `--apply` commands used for the synthetic sample.
Use a distinct test organization/key, and start with `--limit 5` if desired.
Re-running with `--limit 25` imports more of the same manifest and verifies earlier IDs.

`import-state.json` is a checkpoint/report keyed by legacy bill UUID, beside the
manifest by default. It records destination document ID, upload/duplicate/resume
status, extraction outcome, and sanitized errors. `--state PATH` selects another
journal. The journal is bound to the manifest hash, destination origin and API-key
hash; switching any requires a separate state file. The destination organization
is determined by that API key; the legacy company ID does not select it. There is no
API identity lookup in this version, so issue the key for the intended test organization.

All selected files are validated before the first upload, then revalidated before
sending. A file lock prevents simultaneous runs sharing a journal. Uploaded IDs are
checkpointed before extraction. If an upload committed but its response/journal was
lost, a retry uses Payvy's duplicate conflict, then verifies the returned document ID,
SHA-256 and size through `document.get`. It never trusts a conflict message alone.
The current conflict-message format is required; if it changes, the importer fails
that entry. Previously filed/trashed documents stay in that state and are not extracted.

Remote origins require HTTPS; redirects are refused to avoid forwarding the API key.
Bundles and reports are private files (directories 0700, files 0600); keep their parent
private too. `.legacy-import/` is gitignored. File errors continue to the next entry;
exit code 1 indicates failed uploads or extraction needing review. Export skips are
reported separately. A fatal bundle/configuration error stops before uploads.

## Provenance and known limits

The manifest retains legacy company/bill/page IDs, source kind, vendor identity,
invoice number/dates, amount cents and old payment status. Old `Bill.amount`/`paid`
are dollar floats and are converted with decimal half-up rounding. The old paid
amount includes initiated/pending payments, so **it is not settlement evidence**.

Metadata stays in the bundle for comparison; it is not fed to the model or treated
as an extracted answer. Uploaded filenames contain the legacy bill UUID. The app
still calls these documents `source: upload`; it has no import-batch UI or persisted
legacy metadata beyond that filename. Keep the manifest and journal together for
traceability. This version does not restore historic bill dates/statuses/workflows,
verify live legacy storage ahead of export, or automatically compare extraction accuracy.

## Checks

```bash
python3 -m unittest discover -s scripts/legacy_import -v
# Set PAYVY_TEST_ADMIN_URL, PAYVY_TEST_APP_URL and PAYVY_TEST_S3_* for local services:
cargo test -p payvy-server --test legacy_import -- --nocapture
```

The integration test launches the actual Python scripts against the real HTTP router,
a fresh Postgres database with RLS, and S3-compatible storage. Only model responses
are faked. It covers offline validation, upload, extraction, resume, lost-journal
duplicate recovery, organization isolation, and verifies that no bills, vendors or
payments were created. No real legacy database, customer invoices or model API is used.
