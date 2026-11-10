-- Ramp can be selected and mapped, but cannot submit payments yet.
ALTER TABLE bank_connections DROP CONSTRAINT bank_connections_provider_check;
ALTER TABLE bank_connections ADD CHECK (provider IN ('mercury', 'rho', 'ramp'));
ALTER TABLE bank_connections DROP CONSTRAINT bank_connections_mode_check;
ALTER TABLE bank_connections ADD CHECK (mode IN ('request', 'direct', 'read_only', 'setup'));
ALTER TABLE bank_connections ADD CHECK ((provider = 'ramp') = (mode = 'setup'));

CREATE TABLE ramp_authorizations (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    actor text NOT NULL,
    state_hash text NOT NULL UNIQUE,
    environment text NOT NULL CHECK (environment IN ('production', 'sandbox')),
    client_id text NOT NULL,
    redirect_uri text NOT NULL,
    expires_at timestamptz NOT NULL,
    status text NOT NULL CHECK (status IN ('pending', 'ready', 'failed', 'used')),
    credentials_sealed bytea
);
ALTER TABLE ramp_authorizations ENABLE ROW LEVEL SECURITY;
ALTER TABLE ramp_authorizations FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON ramp_authorizations
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

CREATE TABLE ramp_connections (
    connection_id uuid PRIMARY KEY REFERENCES bank_connections(id),
    org_id uuid NOT NULL REFERENCES organizations(id),
    client_id text NOT NULL,
    details jsonb NOT NULL
);
ALTER TABLE ramp_connections ENABLE ROW LEVEL SECURITY;
ALTER TABLE ramp_connections FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON ramp_connections
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

ALTER TABLE vendor_payees ADD COLUMN instrument_id uuid;
ALTER TABLE vendor_payees ADD COLUMN instrument_fingerprint text;
ALTER TABLE vendor_payees ADD CHECK ((instrument_id IS NULL) = (instrument_fingerprint IS NULL));
