-- Operational health belongs to the existing RLS-scoped Ramp connection.
-- No credentials or provider response bodies are stored here.
ALTER TABLE ramp_connections
    ADD COLUMN health_status text NOT NULL DEFAULT 'unknown'
        CHECK (health_status IN ('unknown', 'connected', 'retrying', 'paused', 'needs_reconnection')),
    ADD COLUMN health_reason text,
    ADD COLUMN access_expires_at timestamptz,
    ADD COLUMN checked_at timestamptz,
    ADD COLUMN next_check_at timestamptz,
    ADD COLUMN next_attempt_at timestamptz,
    ADD COLUMN failures integer NOT NULL DEFAULT 0 CHECK (failures BETWEEN 0 AND 5);
CREATE INDEX ramp_connections_due ON ramp_connections (org_id, next_check_at)
    WHERE health_status IN ('unknown', 'connected', 'retrying');
