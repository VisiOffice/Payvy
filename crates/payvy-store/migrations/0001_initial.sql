-- Every row carries its organization; row-level security keeps one
-- organization's data out of another's queries. The app sets
-- `payvy.org_id` at the start of each transaction (Store::begin); a query
-- outside one fails instead of seeing every organization.

CREATE TABLE organizations (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Single-organization installs (self-hosting, development) use this one.
INSERT INTO organizations (id, name)
VALUES ('00000000-0000-0000-0000-000000000001', 'Default');

-- People and agents. Only a SHA-256 of each key is stored. No row-level
-- security: a key is looked up before its organization is known.
CREATE TABLE api_keys (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    actor text NOT NULL,
    kind text NOT NULL CHECK (kind IN ('person', 'agent')),
    key_hash text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);

-- A confirm token commits once. Looked up inside the command's transaction.
CREATE TABLE used_confirm_tokens (
    token text PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    command text NOT NULL,
    used_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE vendors (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    name text NOT NULL,
    email text,
    terms_days integer CHECK (terms_days BETWEEN 0 AND 365),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Account numbers a vendor uses for you, each optionally tied to a location
-- (a job site, a store). Matching uses them to place an invoice.
CREATE TABLE vendor_accounts (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    account_number text NOT NULL,
    location text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (org_id, vendor_id, account_number)
);

-- Workflow versions. A row is never edited except to move its status:
-- proposed -> active -> superseded, or proposed -> declined. At most one
-- active row per vendor, and one for the organization default (vendor_id
-- NULL). Bills point at the exact row they arrived under.
CREATE TABLE workflows (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid REFERENCES vendors(id),
    definition jsonb NOT NULL,
    status text NOT NULL CHECK (status IN ('proposed', 'active', 'superseded', 'declined')),
    reason text,
    proposed_by text NOT NULL,
    decided_by text,
    decided_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX workflows_one_active
    ON workflows (org_id, coalesce(vendor_id, '00000000-0000-0000-0000-000000000000'))
    WHERE status = 'active';

CREATE TABLE bills (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    -- NULL: the built-in default workflow (nothing was ever set).
    workflow_id uuid REFERENCES workflows(id),
    invoice_number text NOT NULL,
    invoice_key text NOT NULL,
    invoice_date date NOT NULL,
    due_date date,
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    memo text,
    external_ref text,
    state text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'rejected', 'void', 'paid')),
    received_at timestamptz NOT NULL DEFAULT now(),
    released_at timestamptz,
    released_by text,
    paid_on date,
    paid_method text,
    paid_reference text,
    closed_reason text,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
-- One live bill per vendor invoice; a voided one frees the number.
CREATE UNIQUE INDEX bills_no_duplicates ON bills (org_id, vendor_id, invoice_key)
    WHERE state <> 'void';
CREATE UNIQUE INDEX bills_external_ref ON bills (org_id, external_ref)
    WHERE external_ref IS NOT NULL AND state <> 'void';
CREATE INDEX bills_vendor ON bills (vendor_id, created_at);

CREATE TABLE bill_lines (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    bill_id uuid NOT NULL REFERENCES bills(id),
    position integer NOT NULL,
    description text NOT NULL,
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    account text,
    UNIQUE (bill_id, position)
);

-- Facts the workflow is evaluated from. Append-only.
CREATE TABLE approvals (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    bill_id uuid NOT NULL REFERENCES bills(id),
    step_id text NOT NULL,
    actor text NOT NULL,
    note text,
    at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bill_id, step_id, actor)
);

CREATE TABLE signals (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    bill_id uuid NOT NULL REFERENCES bills(id),
    key text NOT NULL,
    actor text NOT NULL,
    note text,
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX signals_bill ON signals (bill_id);

-- Every state change appends an event in the same transaction. The audit
-- trail, the bill timeline and future webhooks all read from here.
CREATE TABLE events (
    id bigserial PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    kind text NOT NULL,
    entity_id uuid,
    actor text NOT NULL,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX events_org ON events (org_id, id);
CREATE INDEX events_entity ON events (entity_id, id);

DO $$
DECLARE t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['used_confirm_tokens', 'vendors', 'vendor_accounts', 'workflows', 'bills',
                             'bill_lines', 'approvals', 'signals', 'events'] LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        -- FORCE: the app usually connects as the table owner, which RLS
        -- otherwise exempts. Superusers still bypass it.
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY org_isolation ON %I USING (org_id = current_setting(''payvy.org_id'')::uuid)',
            t
        );
    END LOOP;
END $$;
