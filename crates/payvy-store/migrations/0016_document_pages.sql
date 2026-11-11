-- Page arrangements (docs/document-pages.md). The original file never
-- changes; a document's pages are an arrangement on top of it: the order,
-- each page's rotation and whether it is hidden. Arrangements are versioned
-- like workflows: a change writes a new row, and rows are never updated.
--
-- A document with no row is at version 0: every page in order, upright.
-- Its current version is the highest one here, so there is nothing to keep
-- in step on the document itself.
CREATE TABLE document_arrangements (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    document_id uuid NOT NULL REFERENCES documents(id),
    version integer NOT NULL CHECK (version > 0),
    -- [{page, rotation, hidden}], one entry per page of the original, in
    -- the order they are shown. `page` is the page number in the original.
    pages jsonb NOT NULL CHECK (jsonb_typeof(pages) = 'array'),
    created_at timestamptz NOT NULL,
    created_by text NOT NULL,
    UNIQUE (document_id, version)
);

ALTER TABLE document_arrangements ENABLE ROW LEVEL SECURITY;
ALTER TABLE document_arrangements FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON document_arrangements
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

-- How many pages the original has, read from the file once. NULL: not
-- counted yet (documents from before this migration are counted the first
-- time someone opens them). 0: the file couldn't be read as a PDF, so its
-- pages can't be arranged. Images are one page.
ALTER TABLE documents ADD COLUMN page_count integer CHECK (page_count >= 0);

-- A bill pins the arrangement it uses, as it pins its workflow. NULL: the
-- original arrangement (version 0).
ALTER TABLE bills ADD COLUMN arrangement_id uuid REFERENCES document_arrangements(id);

-- The arrangement version a reading was made from, so a reading older than
-- the current arrangement can be marked out of date.
ALTER TABLE extractions ADD COLUMN arrangement_version integer NOT NULL DEFAULT 0;
