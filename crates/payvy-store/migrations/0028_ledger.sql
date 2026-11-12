-- The books a company posts to. Payvy stays accounts payable: the bank
-- already moved the money, and these rows are the link to the ledger.
-- Currency is checked in the command, not here, so a non-USD connection can
-- be stored and refused with a named error. Credentials are sealed bytes,
-- the same shape as bank_connections.token_sealed. Nothing here is a raw token.
--
-- ledger_files is the PDF's id in the ledger so a retry does not upload a
-- second copy. Bill and payment posting fill the links; the file row waits
-- for the attachment step.

CREATE TABLE ledger_connections (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    provider text NOT NULL CHECK (provider IN ('netsuite', 'visibooks', 'qbo', 'xero')),
    environment text NOT NULL CHECK (environment IN ('production', 'sandbox')),
    -- The ledger's own account id (a NetSuite account id), not a bank account.
    account_id text NOT NULL,
    subsidiary_id text,
    subsidiary_name text,
    currency text NOT NULL,
    status text NOT NULL CHECK (status IN ('active', 'needs_auth', 'disabled')),
    auth_kind text NOT NULL CHECK (auth_kind IN ('authorization_code', 'client_credentials')),
    token_sealed bytea NOT NULL,
    post_when text NOT NULL CHECK (post_when IN ('approval', 'payment')),
    ap_account_id text,
    defaults jsonb NOT NULL DEFAULT '{}',
    -- Payvy bank_connections.id → the ledger's bank or credit-card account id.
    bank_account_map jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL,
    created_by text NOT NULL
);
CREATE UNIQUE INDEX ledger_connections_one_active
    ON ledger_connections (org_id) WHERE status = 'active';

CREATE TABLE ledger_records (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    connection_id uuid NOT NULL REFERENCES ledger_connections(id),
    kind text NOT NULL CHECK (kind IN ('vendor', 'account', 'department', 'class', 'location', 'period')),
    external_id text NOT NULL,
    name text NOT NULL,
    parent_external_id text,
    inactive boolean NOT NULL DEFAULT false,
    detail jsonb NOT NULL DEFAULT '{}',
    UNIQUE (connection_id, kind, external_id)
);

CREATE TABLE ledger_links (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    connection_id uuid NOT NULL REFERENCES ledger_connections(id),
    entity text NOT NULL CHECK (entity IN ('vendor', 'bill', 'payment')),
    -- No foreign key: the column is a vendor, a bill, or a payment.
    payvy_id uuid NOT NULL,
    external_id text,
    external_key text NOT NULL,
    status text NOT NULL CHECK (status IN ('linked', 'posted', 'error', 'voided')),
    error_code text,
    error_detail text,
    request_hash text,
    posted_at timestamptz,
    UNIQUE (org_id, connection_id, entity, payvy_id)
);

CREATE TABLE ledger_files (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    connection_id uuid NOT NULL REFERENCES ledger_connections(id),
    bill_id uuid NOT NULL REFERENCES bills(id),
    external_id text NOT NULL,
    name text NOT NULL,
    UNIQUE (connection_id, bill_id)
);

ALTER TABLE ledger_connections ENABLE ROW LEVEL SECURITY;
ALTER TABLE ledger_connections FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON ledger_connections
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

ALTER TABLE ledger_records ENABLE ROW LEVEL SECURITY;
ALTER TABLE ledger_records FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON ledger_records
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

ALTER TABLE ledger_links ENABLE ROW LEVEL SECURITY;
ALTER TABLE ledger_links FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON ledger_links
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

ALTER TABLE ledger_files ENABLE ROW LEVEL SECURITY;
ALTER TABLE ledger_files FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON ledger_files
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
