-- Invitation delivery is not idempotent at Mercury. Persist an unknown attempt
-- before calling the bank, so a crash or lost response cannot silently resend.
CREATE TABLE vendor_payee_invites (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    connection_id uuid NOT NULL REFERENCES bank_connections(id),
    contact_email text NOT NULL,
    method text NOT NULL CHECK (method IN ('ach', 'check', 'domestic_wire', 'real_time_payment')),
    status text NOT NULL CHECK (status IN ('unknown', 'created', 'completed', 'expired', 'declined', 'failed')),
    mercury_invite_id text,
    proposal_id uuid REFERENCES vendor_payees(id),
    created_at timestamptz NOT NULL,
    created_by text NOT NULL,
    UNIQUE (org_id, connection_id, mercury_invite_id)
);
CREATE UNIQUE INDEX vendor_payee_invites_one_open ON vendor_payee_invites (org_id, vendor_id, connection_id)
    WHERE status IN ('unknown', 'created') OR (status = 'completed' AND proposal_id IS NULL);
ALTER TABLE vendor_payee_invites ENABLE ROW LEVEL SECURITY;
ALTER TABLE vendor_payee_invites FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON vendor_payee_invites USING (org_id = current_setting('payvy.org_id')::uuid);
