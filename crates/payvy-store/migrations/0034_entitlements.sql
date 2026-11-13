-- What a hosted company's plan lets it do (docs/hosted.md). Only the
-- operator's `entitlement.set` writes a row, and only with a higher version
-- than the one stored. Self-hosted installs never read it: everything is on.
-- In hosted mode a company with no row is provisional: reader and payments off.

CREATE TABLE org_entitlements (
    org_id uuid PRIMARY KEY REFERENCES organizations(id),
    reader boolean NOT NULL,
    payments boolean NOT NULL,
    -- When the reader was last switched on. The background reader reads only
    -- documents received since then, so an upgrade doesn't read a backlog at
    -- Payvy's cost; older ones are read when someone asks (document.extract).
    reader_since timestamptz CHECK (reader = (reader_since IS NOT NULL)),
    version bigint NOT NULL CHECK (version > 0),
    updated_at timestamptz NOT NULL,
    updated_by text NOT NULL
);

ALTER TABLE org_entitlements ENABLE ROW LEVEL SECURITY;
ALTER TABLE org_entitlements FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON org_entitlements
    USING (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid)
    WITH CHECK (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid);
