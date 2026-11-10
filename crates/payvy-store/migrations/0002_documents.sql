-- Original files bills come from. The bytes live in object storage under
-- `storage_key`; this row is everything else. One row per distinct file per
-- organization: the same file arriving twice is caught by its hash.
CREATE TABLE documents (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    sha256 text NOT NULL,
    content_type text NOT NULL,
    size_bytes bigint NOT NULL CHECK (size_bytes > 0),
    filename text NOT NULL,
    storage_key text NOT NULL,
    source text NOT NULL CHECK (source IN ('upload', 'email')),
    -- inbox: waiting to become a bill. filed: attached to a bill.
    status text NOT NULL DEFAULT 'inbox' CHECK (status IN ('inbox', 'filed', 'trashed')),
    bill_id uuid REFERENCES bills(id),
    trashed_reason text,
    received_at timestamptz NOT NULL,
    created_by text NOT NULL,
    UNIQUE (org_id, sha256),
    CHECK ((status = 'filed') = (bill_id IS NOT NULL))
);
CREATE INDEX documents_inbox ON documents (org_id, status, received_at);
CREATE INDEX documents_bill ON documents (bill_id);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
ALTER TABLE documents FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON documents USING (org_id = current_setting('payvy.org_id')::uuid);
