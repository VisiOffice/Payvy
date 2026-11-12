-- "Create N bills" on a file of several invoices: the file is split at
-- once, one document per invoice, and each part is then read and made a
-- bill in the background (the server's batch worker), so the person need
-- not keep the page open. One row per request; request_id makes a retry
-- or double click return the same batch instead of splitting twice.
CREATE TABLE bill_batches (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    document_id uuid NOT NULL,
    request_id uuid NOT NULL,
    requested_by text NOT NULL,
    requested_at timestamptz NOT NULL,
    UNIQUE (org_id, request_id),
    FOREIGN KEY (org_id, document_id) REFERENCES documents (org_id, id)
);
CREATE INDEX bill_batches_document ON bill_batches (document_id, requested_at DESC);
ALTER TABLE bill_batches ENABLE ROW LEVEL SECURITY;
ALTER TABLE bill_batches FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON bill_batches
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

-- One part per invoice, in the order the person laid them out. state:
-- reading until the worker decides; then created (a new bill), stacked (a
-- copy of a bill already here, per the invoice stack), needs_you (left in
-- To review with the reason in note), or withdrawn (a person filed, set
-- aside or put back the part first). claimed_until keeps two workers from
-- making the same bill.
CREATE TABLE bill_batch_parts (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    batch_id uuid NOT NULL REFERENCES bill_batches(id),
    position int NOT NULL,
    document_id uuid NOT NULL,
    pages int[] NOT NULL,
    invoice_number text,
    total_cents bigint,
    state text NOT NULL DEFAULT 'reading'
        CHECK (state IN ('reading', 'created', 'stacked', 'needs_you', 'withdrawn')),
    bill_id uuid,
    note text,
    attempts int NOT NULL DEFAULT 0,
    claimed_until timestamptz,
    updated_at timestamptz NOT NULL,
    UNIQUE (batch_id, position),
    FOREIGN KEY (org_id, document_id) REFERENCES documents (org_id, id),
    FOREIGN KEY (org_id, bill_id) REFERENCES bills (org_id, id)
);
CREATE INDEX bill_batch_parts_reading ON bill_batch_parts (org_id) WHERE state = 'reading';
ALTER TABLE bill_batch_parts ENABLE ROW LEVEL SECURITY;
ALTER TABLE bill_batch_parts FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON bill_batch_parts
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
