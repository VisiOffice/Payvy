-- A company's own model key for Payvy's reader (Settings > Agents > Reading).
-- With one, Payvy reads the company's invoices with that key, billed to the
-- company by its provider, whatever its plan's built-in reader. The key is
-- sealed with PAYVY_SECRETS_KEY like a bank token; only its last four
-- characters are kept in the clear, to show which key is set.
CREATE TABLE reader_keys (
    org_id uuid PRIMARY KEY REFERENCES organizations(id),
    provider text NOT NULL CHECK (provider IN ('anthropic', 'openai')),
    model text NOT NULL,
    key_sealed bytea NOT NULL,
    key_last4 text NOT NULL,
    set_at timestamptz NOT NULL,
    set_by text NOT NULL
);

ALTER TABLE reader_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE reader_keys FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON reader_keys
    USING (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid)
    WITH CHECK (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid);
