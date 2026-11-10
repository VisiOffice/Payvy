-- Company-wide approval rules. One row per organization, written the first
-- time a person changes a rule; no row means every rule is off.
CREATE TABLE approval_rules (
    org_id uuid PRIMARY KEY REFERENCES organizations(id),
    -- Whoever entered a bill or uploaded its document can't approve it.
    submitter_cannot_approve boolean NOT NULL DEFAULT false,
    updated_by text NOT NULL,
    updated_at timestamptz NOT NULL
);
ALTER TABLE approval_rules ENABLE ROW LEVEL SECURITY;
ALTER TABLE approval_rules FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON approval_rules
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
