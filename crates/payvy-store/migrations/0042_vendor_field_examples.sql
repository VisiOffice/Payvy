-- Evidence a person tagged (or later accepted from a suggestion) showing what
-- on a vendor's invoice ties it to a purchase order (or, in future, other
-- fields). Phase 1 writes and reads only po_reference, service_address, and
-- vendor_reference.
CREATE TABLE vendor_field_examples (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    bill_id uuid NOT NULL REFERENCES bills(id),
    document_id uuid REFERENCES documents(id),
    po_id uuid REFERENCES purchase_orders(id),
    field text NOT NULL CHECK (field IN ('po_reference', 'service_address', 'vendor_reference')),
    quote text NOT NULL,
    value text NOT NULL,
    page integer CHECK (page IS NULL OR page > 0),
    grounded boolean NOT NULL DEFAULT false,
    source text NOT NULL CHECK (source IN ('person_tag', 'suggestion_accepted')),
    created_by text NOT NULL,
    created_at timestamptz NOT NULL,
    retired_by text,
    retired_at timestamptz,
    retired_reason text,
    CHECK ((retired_at IS NULL) = (retired_by IS NULL))
);

CREATE INDEX vendor_field_examples_live
    ON vendor_field_examples (org_id, vendor_id, created_at) WHERE retired_at IS NULL;

ALTER TABLE vendor_field_examples ENABLE ROW LEVEL SECURITY;
ALTER TABLE vendor_field_examples FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON vendor_field_examples
    USING (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid)
    WITH CHECK (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid);
