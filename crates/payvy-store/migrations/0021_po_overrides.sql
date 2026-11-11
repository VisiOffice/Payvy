-- Scoped exception overrides (PO Matching Spec v2, step 2) and the explicit
-- customer-funding gate flag. An override is bound to one bill, the selected
-- POs/allocations fingerprint, the exception kind and an authorized excess;
-- any relevant change makes the fingerprint differ and voids it.
CREATE TABLE po_overrides (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    bill_id uuid NOT NULL REFERENCES bills(id),
    exception_kind text NOT NULL,
    authorized_excess_cents bigint NOT NULL CHECK (authorized_excess_cents >= 0),
    bill_amount_cents bigint NOT NULL,
    fingerprint text NOT NULL,
    reason text NOT NULL,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL
);
CREATE INDEX po_overrides_bill ON po_overrides (bill_id, created_at DESC);
ALTER TABLE po_overrides ENABLE ROW LEVEL SECURITY;
ALTER TABLE po_overrides FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON po_overrides
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

ALTER TABLE vendor_po_settings
    ADD COLUMN require_funding boolean NOT NULL DEFAULT false;

ALTER TABLE po_overrides ADD CONSTRAINT po_overrides_bill_org
    FOREIGN KEY (org_id, bill_id) REFERENCES bills (org_id, id);

-- The company-wide purchase-orders switch. No row means off: every po.*
-- write is refused and the payment gate does nothing, so turning POs off
-- never blocks a payment. When on, "PO required" and overage caps stay per
-- vendor in vendor_po_settings.
CREATE TABLE po_company_settings (
    org_id uuid PRIMARY KEY REFERENCES organizations(id),
    enabled boolean NOT NULL DEFAULT false,
    updated_at timestamptz NOT NULL,
    updated_by text NOT NULL
);
ALTER TABLE po_company_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE po_company_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON po_company_settings
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
