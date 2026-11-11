-- The migration owner needs to validate cross-company constraints before RLS is forced again.
ALTER TABLE bills NO FORCE ROW LEVEL SECURITY;
ALTER TABLE documents NO FORCE ROW LEVEL SECURITY;
ALTER TABLE extractions NO FORCE ROW LEVEL SECURITY;
-- No rewriting old bills. Signed adjustments still reconcile to a positive total.
ALTER TABLE bill_lines DROP CONSTRAINT bill_lines_amount_cents_check;
ALTER TABLE extractions ADD CONSTRAINT extractions_org_id_id UNIQUE (org_id, id);
CREATE TABLE invoice_review_settings (
    org_id uuid PRIMARY KEY REFERENCES organizations(id),
    enabled boolean NOT NULL DEFAULT false
);
CREATE TABLE invoice_reviews (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    document_id uuid NOT NULL,
    extraction_id uuid,
    version integer NOT NULL CHECK (version > 0),
    arrangement_version integer NOT NULL CHECK (arrangement_version >= 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    summary_reason text,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL,
    request_id uuid NOT NULL,
    request_payload jsonb NOT NULL,
    UNIQUE (org_id, id),
    UNIQUE (org_id, document_id, version),
    UNIQUE (org_id, created_by, request_id),
    FOREIGN KEY (org_id, document_id) REFERENCES documents(org_id, id),
    FOREIGN KEY (org_id, extraction_id) REFERENCES extractions(org_id, id)
);
-- Separate link so old bills need no backfill and a revision can make only one bill.
CREATE TABLE bill_invoice_reviews (
    org_id uuid NOT NULL REFERENCES organizations(id),
    bill_id uuid PRIMARY KEY REFERENCES bills(id),
    review_id uuid NOT NULL UNIQUE,
    FOREIGN KEY (org_id, review_id) REFERENCES invoice_reviews(org_id, id)
);
-- bills has no composite key on current main; enforce the link at the database too.
ALTER TABLE bills ADD CONSTRAINT bills_org_review_id UNIQUE (org_id, id);
ALTER TABLE bill_invoice_reviews ADD FOREIGN KEY (org_id, bill_id) REFERENCES bills(org_id, id);
ALTER TABLE invoice_review_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE invoice_review_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON invoice_review_settings USING (org_id = current_setting('payvy.org_id')::uuid);
ALTER TABLE invoice_reviews ENABLE ROW LEVEL SECURITY;
ALTER TABLE invoice_reviews FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON invoice_reviews USING (org_id = current_setting('payvy.org_id')::uuid);
ALTER TABLE bill_invoice_reviews ENABLE ROW LEVEL SECURITY;
ALTER TABLE bill_invoice_reviews FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON bill_invoice_reviews USING (org_id = current_setting('payvy.org_id')::uuid);

ALTER TABLE bills FORCE ROW LEVEL SECURITY;
ALTER TABLE documents FORCE ROW LEVEL SECURITY;
ALTER TABLE extractions FORCE ROW LEVEL SECURITY;
