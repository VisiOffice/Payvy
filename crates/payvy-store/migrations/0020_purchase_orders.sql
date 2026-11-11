-- Purchase orders and the allocation ledger (PO Matching Spec v2, step 1:
-- lifecycle + amounts). Lifecycle (draft/active/closed/canceled) is explicit
-- and person-driven; billing progress is derived in payvy-domain and never
-- stored. Amounts are authorized / reserved / consumed / available, derived
-- from po_allocations (see payvy-store::purchase_orders::amounts).
CREATE TABLE purchase_orders (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    po_number text NOT NULL,
    status text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'closed', 'canceled')),
    currency text NOT NULL DEFAULT 'USD' CHECK (currency = 'USD'),
    owner text,
    activated_by text,
    external_source text,
    external_id text,
    service_address text,
    period_start date,
    period_end date,
    lifecycle_reason text,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (org_id, po_number)
);
CREATE UNIQUE INDEX purchase_orders_external
    ON purchase_orders (org_id, external_source, external_id)
    WHERE external_source IS NOT NULL;
ALTER TABLE purchase_orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE purchase_orders FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON purchase_orders
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

-- One immutable snapshot per activation/change. Allocations pin the revision
-- they were made against; a new revision never releases existing allocations.
CREATE TABLE po_revisions (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    po_id uuid NOT NULL REFERENCES purchase_orders(id),
    revision integer NOT NULL CHECK (revision >= 1),
    authorized_cents bigint NOT NULL CHECK (authorized_cents > 0),
    -- Overage allowance pinned in cents at this revision: the smaller of the
    -- vendor's dollar/percentage caps (payvy-domain::purchase_orders).
    -- NULL caps mean "not configured"; the allowance is then 0.
    overage_cents_cap bigint CHECK (overage_cents_cap >= 0),
    overage_percent_bp integer CHECK (overage_percent_bp >= 0),
    allowance_cents bigint NOT NULL CHECK (allowance_cents >= 0),
    reason text,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL,
    UNIQUE (po_id, revision)
);
ALTER TABLE po_revisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE po_revisions FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON po_revisions
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

CREATE TABLE po_lines (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    po_id uuid NOT NULL REFERENCES purchase_orders(id),
    revision_id uuid NOT NULL REFERENCES po_revisions(id),
    position integer NOT NULL,
    description text NOT NULL,
    quantity double precision,
    unit text,
    unit_price_cents bigint,
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    account text,
    period_start date,
    period_end date
);
ALTER TABLE po_lines ENABLE ROW LEVEL SECURITY;
ALTER TABLE po_lines FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON po_lines
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

-- The allocation ledger. suggested reserves nothing (previews are ephemeral
-- and not stored; a row starts at reserved). released is reached only by a
-- reversal (release_reason), never by deletion and never by a PO revision.
CREATE TABLE po_allocations (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    po_id uuid NOT NULL REFERENCES purchase_orders(id),
    revision_id uuid NOT NULL REFERENCES po_revisions(id),
    bill_id uuid REFERENCES bills(id),
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    state text NOT NULL CHECK (state IN ('reserved', 'consumed', 'released')),
    kind text NOT NULL DEFAULT 'match' CHECK (kind IN ('match', 'opening_consumption')),
    source_actor text NOT NULL,
    source_external_source text,
    source_external_id text,
    source_allocation_id text,
    reasons jsonb NOT NULL DEFAULT '[]',
    created_at timestamptz NOT NULL,
    accepted_at timestamptz,
    accepted_by text,
    released_at timestamptz,
    released_by text,
    release_reason text
);
CREATE UNIQUE INDEX po_allocations_opening_source
    ON po_allocations (org_id, source_external_source, source_external_id, source_allocation_id)
    WHERE kind = 'opening_consumption' AND source_allocation_id IS NOT NULL;
CREATE INDEX po_allocations_po_state ON po_allocations (po_id, state);
ALTER TABLE po_allocations ENABLE ROW LEVEL SECURITY;
ALTER TABLE po_allocations FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON po_allocations
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

-- Per-vendor PO configuration: whether a PO is required, and overage caps.
-- Caps are snapshotted (pinned) onto each PO revision; they are not the
-- allowance itself.
CREATE TABLE vendor_po_settings (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    require_po boolean NOT NULL DEFAULT false,
    overage_cents_cap bigint CHECK (overage_cents_cap >= 0),
    overage_percent_bp integer CHECK (overage_percent_bp >= 0),
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL,
    UNIQUE (org_id, vendor_id)
);
ALTER TABLE vendor_po_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE vendor_po_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON vendor_po_settings
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

-- Cross-company links are tied to the organization (pattern from
-- 0018_document_split): a PO, revision, line or allocation can only name
-- records of its own organization, enforced by the foreign keys themselves.
ALTER TABLE vendors NO FORCE ROW LEVEL SECURITY;
ALTER TABLE vendors ADD CONSTRAINT vendors_org_id_id UNIQUE (org_id, id);
ALTER TABLE vendors FORCE ROW LEVEL SECURITY;
ALTER TABLE bills NO FORCE ROW LEVEL SECURITY;
ALTER TABLE bills ADD CONSTRAINT bills_org_id_id UNIQUE (org_id, id);
ALTER TABLE bills FORCE ROW LEVEL SECURITY;

ALTER TABLE purchase_orders ADD CONSTRAINT purchase_orders_org_id_id UNIQUE (org_id, id);
ALTER TABLE purchase_orders ADD CONSTRAINT purchase_orders_vendor_org
    FOREIGN KEY (org_id, vendor_id) REFERENCES vendors (org_id, id);
ALTER TABLE po_revisions ADD CONSTRAINT po_revisions_org_id_id UNIQUE (org_id, id);
ALTER TABLE po_revisions ADD CONSTRAINT po_revisions_po_org
    FOREIGN KEY (org_id, po_id) REFERENCES purchase_orders (org_id, id);
ALTER TABLE po_lines ADD CONSTRAINT po_lines_po_org
    FOREIGN KEY (org_id, po_id) REFERENCES purchase_orders (org_id, id);
ALTER TABLE po_lines ADD CONSTRAINT po_lines_revision_org
    FOREIGN KEY (org_id, revision_id) REFERENCES po_revisions (org_id, id);
ALTER TABLE po_allocations ADD CONSTRAINT po_allocations_po_org
    FOREIGN KEY (org_id, po_id) REFERENCES purchase_orders (org_id, id);
ALTER TABLE po_allocations ADD CONSTRAINT po_allocations_revision_org
    FOREIGN KEY (org_id, revision_id) REFERENCES po_revisions (org_id, id);
ALTER TABLE po_allocations ADD CONSTRAINT po_allocations_bill_org
    FOREIGN KEY (org_id, bill_id) REFERENCES bills (org_id, id);
ALTER TABLE vendor_po_settings ADD CONSTRAINT vendor_po_settings_vendor_org
    FOREIGN KEY (org_id, vendor_id) REFERENCES vendors (org_id, id);
