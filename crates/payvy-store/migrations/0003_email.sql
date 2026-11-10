-- Email intake. Each organization has one live inbound address,
-- `{token}@{PAYVY_INBOUND_DOMAIN}`. The webhook looks the token up before it
-- knows the organization, so this table has no row-level security, like
-- api_keys. A retired token stops routing; mail to it is ignored.
CREATE TABLE inbound_addresses (
    token text PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    created_at timestamptz NOT NULL,
    created_by text NOT NULL,
    retired_at timestamptz
);
CREATE UNIQUE INDEX inbound_addresses_live ON inbound_addresses (org_id) WHERE retired_at IS NULL;

-- One row per email received. The raw message is kept in object storage
-- under `storage_key`; its attachments become documents. The same email
-- delivered twice (a retry, or sent to two of our addresses) is caught by
-- its Message-ID or, without one, by the hash of the raw bytes.
CREATE TABLE emails (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    message_id text,
    raw_sha256 text NOT NULL,
    size_bytes bigint NOT NULL CHECK (size_bytes > 0),
    storage_key text NOT NULL,
    recipient text,
    from_address text,
    from_name text,
    subject text,
    sent_at timestamptz,
    authentication_results text,
    -- [{filename, reason}]: parts that didn't become documents, and why.
    skipped jsonb NOT NULL DEFAULT '[]',
    received_at timestamptz NOT NULL,
    created_by text NOT NULL,
    UNIQUE (org_id, raw_sha256)
);
CREATE UNIQUE INDEX emails_message_id ON emails (org_id, message_id) WHERE message_id IS NOT NULL;
CREATE INDEX emails_received ON emails (org_id, received_at);

ALTER TABLE emails ENABLE ROW LEVEL SECURITY;
ALTER TABLE emails FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON emails USING (org_id = current_setting('payvy.org_id')::uuid);

ALTER TABLE documents ADD COLUMN email_id uuid REFERENCES emails(id);
ALTER TABLE documents ADD CONSTRAINT documents_email_source CHECK ((source = 'email') = (email_id IS NOT NULL));
CREATE INDEX documents_email ON documents (email_id);
