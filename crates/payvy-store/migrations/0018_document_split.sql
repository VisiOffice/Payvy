-- Splitting documents (docs/document-pages.md, step 2). A document split
-- from another shares its original file: the same storage key and hash, no
-- copy. Its own arrangement lists only its pages, by their numbers in that
-- file, so it can never show the others.
--
-- Checking the new constraints reads every row, which the forced policy
-- would refuse outside an organization's scope; the owner reads them all
-- for the length of this migration, and the policy is forced again below.
ALTER TABLE documents NO FORCE ROW LEVEL SECURITY;

-- Lineage within one organization: (org_id, derived_from) must name a
-- document of the same organization.
ALTER TABLE documents ADD CONSTRAINT documents_org_id_id UNIQUE (org_id, id);
ALTER TABLE documents ADD COLUMN derived_from uuid;
ALTER TABLE documents ADD CONSTRAINT documents_derived_from_fkey
    FOREIGN KEY (org_id, derived_from) REFERENCES documents (org_id, id);
CREATE INDEX documents_derived_from ON documents (derived_from) WHERE derived_from IS NOT NULL;

-- "This exact file is already here" holds for files that arrived, not for
-- the documents split from them, which share their hash.
ALTER TABLE documents DROP CONSTRAINT documents_org_id_sha256_key;
CREATE UNIQUE INDEX documents_original_sha256 ON documents (org_id, sha256)
    WHERE derived_from IS NULL;

-- split: every page moved to other documents (split off, or returned to the
-- document it came from by an undo). It has no pages left and leaves To review.
ALTER TABLE documents DROP CONSTRAINT documents_status_check;
ALTER TABLE documents ADD CONSTRAINT documents_status_check
    CHECK (status IN ('inbox', 'filed', 'trashed', 'split'));

ALTER TABLE documents FORCE ROW LEVEL SECURITY;

-- The invoices a reading found in the file, when there were several and
-- their pages checked out: [{pages, invoice_number, total, total_cents}].
-- NULL for a single invoice.
ALTER TABLE extractions ADD COLUMN invoices jsonb CHECK (invoices IS NULL OR jsonb_typeof(invoices) = 'array');
