-- Each time a model read a document. `answer` is what the model returned,
-- as printed on the page; `draft`, `checks` and `vendor_match` are Payvy's
-- reading of it at the time, kept so a reviewer sees what the agent saw.
-- A failed attempt is a row too, so errors are visible and retries back off.
CREATE TABLE extractions (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    document_id uuid NOT NULL REFERENCES documents(id),
    status text NOT NULL CHECK (status IN ('read', 'failed')),
    provider text NOT NULL,
    model text NOT NULL,
    answer jsonb,
    draft jsonb,
    checks jsonb,
    vendor_match jsonb,
    error text,
    created_at timestamptz NOT NULL,
    created_by text NOT NULL,
    CHECK ((status = 'read') = (answer IS NOT NULL AND draft IS NOT NULL AND checks IS NOT NULL)),
    CHECK ((status = 'failed') = (error IS NOT NULL))
);
CREATE INDEX extractions_document ON extractions (document_id, created_at);

ALTER TABLE extractions ENABLE ROW LEVEL SECURITY;
ALTER TABLE extractions FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON extractions USING (org_id = current_setting('payvy.org_id')::uuid);
