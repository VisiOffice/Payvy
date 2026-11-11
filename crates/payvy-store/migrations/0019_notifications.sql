CREATE TABLE notification_settings (
    org_id uuid PRIMARY KEY REFERENCES organizations(id),
    enabled boolean NOT NULL DEFAULT false,
    reminders boolean NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL,
    updated_by text NOT NULL
);
CREATE TABLE notifications (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    bill_id uuid NOT NULL REFERENCES bills(id),
    person_id uuid NOT NULL REFERENCES people(id),
    actor text NOT NULL,
    email text NOT NULL,
    step_id text NOT NULL,
    action text NOT NULL CHECK (action IN ('approve', 'release')),
    reminder integer NOT NULL CHECK (reminder BETWEEN 0 AND 3),
    status text NOT NULL CHECK (status IN ('queued','sending','sent','logged','failed','unknown','cancelled')),
    attempts integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL,
    next_attempt_at timestamptz NOT NULL,
    attempted_at timestamptz,
    finished_at timestamptz,
    reason text,
    UNIQUE (org_id, bill_id, step_id, person_id, reminder)
);
CREATE INDEX notifications_pending ON notifications (org_id, next_attempt_at, id) WHERE status IN ('queued','sending');
CREATE INDEX notifications_bill ON notifications (org_id, bill_id, created_at);
ALTER TABLE notification_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE notification_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON notification_settings USING (org_id = current_setting('payvy.org_id', true)::uuid) WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
ALTER TABLE notifications ENABLE ROW LEVEL SECURITY;
ALTER TABLE notifications FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON notifications USING (org_id = current_setting('payvy.org_id', true)::uuid) WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
