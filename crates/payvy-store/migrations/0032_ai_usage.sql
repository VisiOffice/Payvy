-- Durable record of every AI extraction attempt, written before provider
-- dispatch so token usage and cost survive parse failures, validation errors,
-- timeouts, or downstream database failures.

CREATE TABLE ai_usage_attempts (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    document_id uuid,
    actor_kind text NOT NULL CHECK (actor_kind IN ('person', 'agent')),
    actor_name text NOT NULL,
    provider text NOT NULL,
    requested_model text NOT NULL,
    actual_model text,
    provider_request_id text,
    outcome text NOT NULL CHECK (outcome IN (
        'pending',
        'succeeded',
        'refused',
        'truncated',
        'invalid_output',
        'provider_error',
        'timeout',
        'unsupported'
    )),
    error text,
    input_tokens bigint,
    output_tokens bigint,
    cache_read_tokens bigint,
    cache_write_tokens bigint,
    currency text NOT NULL DEFAULT 'USD',
    pricing_version text,
    -- 1/1,000,000 of a cent (1/100,000,000 USD); NULL when usage or pricing is unknown.
    cost_microcents bigint,
    started_at timestamptz NOT NULL,
    completed_at timestamptz,
    FOREIGN KEY (org_id, document_id) REFERENCES documents (org_id, id) ON DELETE SET NULL (document_id)
);

CREATE INDEX ai_usage_attempts_org_started
    ON ai_usage_attempts (org_id, started_at);

CREATE INDEX ai_usage_attempts_document_started
    ON ai_usage_attempts (document_id, started_at);

ALTER TABLE ai_usage_attempts ENABLE ROW LEVEL SECURITY;
ALTER TABLE ai_usage_attempts FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON ai_usage_attempts
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
