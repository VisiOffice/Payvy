-- Quarry's purchase-order feed. Follows 0028, the ledger tables from #41.
-- This migration does not read those tables.
--
-- A standing authorization lets one Admin approve one agent key for a source
-- (Quarry) and named vendors, up to a per-PO amount. Only that key's upsert
-- activates a PO, and the source is the one on the authorization. Any other
-- caller leaves a draft. Revoking stops later automatic activation and
-- leaves POs already activated. person_hold records that a person edited the
-- PO, so a later sync cannot change it. vendor_reference, metadata and
-- final_period are what Allied matching reads. body_hash is the last upsert
-- body, so a repeat is a no-op.

ALTER TABLE purchase_orders
    ADD COLUMN vendor_reference text,
    ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}',
    ADD COLUMN final_period boolean NOT NULL DEFAULT false,
    ADD COLUMN body_hash text,
    ADD COLUMN person_hold boolean NOT NULL DEFAULT false;

CREATE TABLE vendor_external_ids (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL,
    source text NOT NULL,
    external_id text NOT NULL,
    linked_by text NOT NULL,
    linked_at timestamptz NOT NULL,
    UNIQUE (org_id, id),
    UNIQUE (org_id, source, external_id),
    UNIQUE (org_id, vendor_id, source)
);
ALTER TABLE vendor_external_ids ENABLE ROW LEVEL SECURITY;
ALTER TABLE vendor_external_ids FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON vendor_external_ids
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

CREATE TABLE po_source_authorizations (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    source text NOT NULL,
    -- Actor name of the agent key. Activation and external close/cancel
    -- require this caller; the request does not get to choose the source.
    agent text NOT NULL,
    max_cents bigint NOT NULL CHECK (max_cents > 0),
    owner text NOT NULL,
    authorized_by text NOT NULL,
    created_at timestamptz NOT NULL,
    revoked_at timestamptz,
    revoked_by text,
    UNIQUE (org_id, id)
);
ALTER TABLE po_source_authorizations ENABLE ROW LEVEL SECURITY;
ALTER TABLE po_source_authorizations FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON po_source_authorizations
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

CREATE TABLE po_source_authorization_vendors (
    org_id uuid NOT NULL REFERENCES organizations(id),
    authorization_id uuid NOT NULL,
    vendor_id uuid NOT NULL,
    PRIMARY KEY (authorization_id, vendor_id)
);
ALTER TABLE po_source_authorization_vendors ENABLE ROW LEVEL SECURITY;
ALTER TABLE po_source_authorization_vendors FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON po_source_authorization_vendors
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

ALTER TABLE vendors NO FORCE ROW LEVEL SECURITY;
ALTER TABLE po_source_authorizations NO FORCE ROW LEVEL SECURITY;
ALTER TABLE vendor_external_ids ADD CONSTRAINT vendor_external_ids_vendor_org
    FOREIGN KEY (org_id, vendor_id) REFERENCES vendors (org_id, id);
ALTER TABLE po_source_authorization_vendors
    ADD CONSTRAINT po_source_authorization_vendors_auth_org
    FOREIGN KEY (org_id, authorization_id) REFERENCES po_source_authorizations (org_id, id);
ALTER TABLE po_source_authorization_vendors
    ADD CONSTRAINT po_source_authorization_vendors_vendor_org
    FOREIGN KEY (org_id, vendor_id) REFERENCES vendors (org_id, id);
ALTER TABLE vendors FORCE ROW LEVEL SECURITY;
ALTER TABLE po_source_authorizations FORCE ROW LEVEL SECURITY;
