-- The enumerated policy registry (PO Matching Spec v2, "Vendor and company
-- policies: the bones"): named policies with typed options, held as data a
-- person sets — never core code, never a scripting surface. Each change is
-- a new immutable version (proposed-and-accepted like a workflow change,
-- collapsed here to a person's direct set); bills pin the version in force
-- when they are created, so a change never rewrites a bill in flight.
-- A bill with no pin (every bill before this migration) reads the defaults.
CREATE TABLE vendor_po_policies (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL,
    version integer NOT NULL CHECK (version >= 1),
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'superseded')),
    -- over_period: flag_only | require_full_cover (payvy-domain::purchase_orders)
    over_period text NOT NULL DEFAULT 'flag_only'
        CHECK (over_period IN ('flag_only', 'require_full_cover')),
    reason text,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL,
    UNIQUE (org_id, id),
    UNIQUE (org_id, vendor_id, version)
);
CREATE UNIQUE INDEX vendor_po_policies_one_active
    ON vendor_po_policies (org_id, vendor_id) WHERE status = 'active';
ALTER TABLE vendor_po_policies ENABLE ROW LEVEL SECURITY;
ALTER TABLE vendor_po_policies FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON vendor_po_policies
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
-- The foreign keys scan their tables before any org is selected, so
-- forced RLS is lifted while they are added (the pattern 0020 uses).
ALTER TABLE vendors NO FORCE ROW LEVEL SECURITY;
ALTER TABLE bills NO FORCE ROW LEVEL SECURITY;
ALTER TABLE vendor_po_policies ADD CONSTRAINT vendor_po_policies_vendor_org
    FOREIGN KEY (org_id, vendor_id) REFERENCES vendors (org_id, id);

ALTER TABLE bills ADD COLUMN policy_version_id uuid;
ALTER TABLE bills ADD CONSTRAINT bills_policy_version_org
    FOREIGN KEY (org_id, policy_version_id) REFERENCES vendor_po_policies (org_id, id);
ALTER TABLE vendors FORCE ROW LEVEL SECURITY;
ALTER TABLE bills FORCE ROW LEVEL SECURITY;

-- A PO can be paused without ending it: existing allocations stand, new
-- ones are refused, and its bills wait until it is reactivated.
ALTER TABLE purchase_orders DROP CONSTRAINT purchase_orders_status_check;
ALTER TABLE purchase_orders ADD CONSTRAINT purchase_orders_status_check
    CHECK (status IN ('draft', 'active', 'suspended', 'closed', 'canceled'));
