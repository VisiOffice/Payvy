-- Paying through the customer's own bank. Payvy holds no money and no bank
-- account numbers: a connection is the customer's API token (sealed with
-- PAYVY_SECRETS_KEY), a payee points at a recipient set up in the bank, and
-- a payment is an instruction the bank carried out or didn't.

CREATE TABLE bank_connections (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    provider text NOT NULL CHECK (provider IN ('mercury')),
    environment text NOT NULL CHECK (environment IN ('production', 'sandbox')),
    -- request: payments wait for approval inside the bank. direct: sent at once.
    mode text NOT NULL CHECK (mode IN ('request', 'direct')),
    account_id text NOT NULL,
    account_name text NOT NULL,
    token_sealed bytea NOT NULL,
    created_at timestamptz NOT NULL,
    created_by text NOT NULL,
    -- A retired connection makes no new payments but still tracks the ones
    -- it made, so its token is kept.
    retired_at timestamptz,
    retired_by text
);
CREATE UNIQUE INDEX bank_connections_one_live ON bank_connections (org_id) WHERE retired_at IS NULL;

-- Who a vendor's bills are paid to: a recipient in the bank and a method.
-- Like workflows, rows only move status: proposed -> active -> superseded,
-- or proposed -> declined. Only a person accepts one.
CREATE TABLE vendor_payees (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    connection_id uuid NOT NULL REFERENCES bank_connections(id),
    recipient_id text NOT NULL,
    recipient_name text NOT NULL,
    method text NOT NULL CHECK (method IN ('ach', 'check', 'domestic_wire', 'real_time_payment')),
    -- What a person checks: the account's last four, or where a check goes.
    destination text NOT NULL,
    status text NOT NULL CHECK (status IN ('proposed', 'active', 'superseded', 'declined')),
    reason text,
    proposed_by text NOT NULL,
    decided_by text,
    decided_at timestamptz,
    created_at timestamptz NOT NULL
);
CREATE UNIQUE INDEX vendor_payees_one_active ON vendor_payees (org_id, vendor_id) WHERE status = 'active';

CREATE TABLE payments (
    id uuid PRIMARY KEY,
    -- Creation order, for listing attempts newest first even within one instant.
    seq bigint GENERATED ALWAYS AS IDENTITY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    bill_id uuid NOT NULL REFERENCES bills(id),
    connection_id uuid NOT NULL REFERENCES bank_connections(id),
    payee_id uuid NOT NULL REFERENCES vendor_payees(id),
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    method text NOT NULL,
    -- Sent with the instruction: the same key can't pay twice.
    idempotency_key text NOT NULL,
    -- submitting: sent without a clear answer; it is sent again under the
    -- same key until the bank answers.
    status text NOT NULL CHECK (status IN ('submitting', 'awaiting_bank_approval', 'pending', 'sent',
                                            'failed', 'cancelled', 'rejected', 'returned')),
    request_id text,
    transaction_id text,
    failure text,
    link text,
    created_at timestamptz NOT NULL,
    created_by text NOT NULL,
    updated_at timestamptz NOT NULL,
    checked_at timestamptz NOT NULL,
    UNIQUE (org_id, idempotency_key)
);
-- At most one payment per bill that is going or gone.
CREATE UNIQUE INDEX payments_one_live ON payments (bill_id)
    WHERE status IN ('submitting', 'awaiting_bank_approval', 'pending', 'sent');
CREATE INDEX payments_open ON payments (org_id, checked_at)
    WHERE status IN ('submitting', 'awaiting_bank_approval', 'pending', 'sent');

DO $$
DECLARE t text;
BEGIN
    FOREACH t IN ARRAY ARRAY['bank_connections', 'vendor_payees', 'payments'] LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY org_isolation ON %I USING (org_id = current_setting(''payvy.org_id'')::uuid)',
            t
        );
    END LOOP;
END $$;
