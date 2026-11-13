-- Idempotent multi-company provisioning by the operator API and CLI.
-- Like organizations, people, and api_keys, this table has no row-level
-- security; only operator provisioning paths access it.

CREATE TABLE org_provisioning (
    idempotency_key text PRIMARY KEY CHECK (length(btrim(idempotency_key)) BETWEEN 1 AND 255),
    request_hash text NOT NULL,
    org_id uuid NOT NULL UNIQUE REFERENCES organizations(id),
    admin_person_id uuid NOT NULL REFERENCES people(id),
    created_at timestamptz NOT NULL
);

-- Cross-company AI usage reporting for the operator API only.
-- Replace org_isolation with NULLIF(..., '') so pooled connections whose
-- previous transaction reset `payvy.org_id` to '' evaluate cleanly to NULL
-- instead of failing `''::uuid` when OR'd with `operator_ai_usage_read`.
DROP POLICY org_isolation ON ai_usage_attempts;
CREATE POLICY org_isolation ON ai_usage_attempts
    USING (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid)
    WITH CHECK (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid);

-- SELECT-only: inserts, updates, and deletes still require org_isolation (payvy.org_id).
CREATE POLICY operator_ai_usage_read ON ai_usage_attempts
    FOR SELECT
    USING (current_setting('payvy.operator', true) = 'true');
