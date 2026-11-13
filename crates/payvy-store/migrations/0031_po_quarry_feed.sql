-- Quarry's purchase-order feed. This is 0031. The VisiBooks branch
-- takes 0029 and 0030 and merges first. This migration does not read
-- the ledger tables.
--
-- A standing authorization binds one agent key, by api_keys.id, to a
-- source and named vendors, up to a per-PO amount. Only that key's
-- upsert creates or revises a PO for the source, and only for those
-- vendors. It also activates within the limit. The source is the one
-- stored here. Revoking stops later automatic activation and leaves
-- POs already activated. person_hold records that a person edited the
-- PO, so a later sync cannot change it until a person clears the hold.
-- vendor_reference, metadata and final_period are what Allied matching
-- reads. body_hash is the last upsert body, so a repeat is a no-op.

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

-- (org_id, id) so an authorization can reference a key in its own company.
ALTER TABLE api_keys
    ADD CONSTRAINT api_keys_org_id_id UNIQUE (org_id, id);

CREATE TABLE po_source_authorizations (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    source text NOT NULL,
    -- The agent key. Create, revision, activation, and external close
    -- or cancel require this key, and the vendor must be one it covers.
    key_id uuid NOT NULL,
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
ALTER TABLE po_source_authorizations
    ADD CONSTRAINT po_source_authorizations_key_org
    FOREIGN KEY (org_id, key_id) REFERENCES api_keys (org_id, id);
ALTER TABLE vendors FORCE ROW LEVEL SECURITY;
ALTER TABLE po_source_authorizations FORCE ROW LEVEL SECURITY;
