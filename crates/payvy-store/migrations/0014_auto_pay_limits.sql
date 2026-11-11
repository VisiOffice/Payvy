-- The ceiling on automatic payments. One row with vendor_id NULL is the
-- company's limit; a row per vendor overrides it, higher or lower. With no company
-- row, the default in payvy-domain applies ($5,000.00,
-- workflow::DEFAULT_AUTO_PAY_LIMIT_CENTS), so existing and new companies
-- start with the same ceiling. There is no "no limit".
CREATE TABLE auto_pay_limits (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid REFERENCES vendors(id),
    limit_cents bigint NOT NULL CHECK (limit_cents BETWEEN 0 AND 100000000000),
    set_by text NOT NULL,
    set_at timestamptz NOT NULL
);
CREATE UNIQUE INDEX auto_pay_limits_one_per_scope
    ON auto_pay_limits (org_id, coalesce(vendor_id, '00000000-0000-0000-0000-000000000000'));
ALTER TABLE auto_pay_limits ENABLE ROW LEVEL SECURITY;
ALTER TABLE auto_pay_limits FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON auto_pay_limits
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
