-- Automatic ledger posting runs as queued jobs, never inside the command
-- that approved or paid: that transaction commits its own facts first,
-- and a worker posts afterwards in its own transaction (hooks used to
-- call the ledger inside the command transaction, where a slow or
-- failing ledger could stall or roll back a bank fact). A failure is
-- recorded on the job and on the bill's ledger link, and retried with
-- backoff; a person can always post by hand in the meantime.
CREATE TABLE ledger_jobs (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    kind text NOT NULL CHECK (kind IN ('post_bill', 'post_payment', 'bank_reference')),
    bill_id uuid,
    payment_id uuid,
    payload jsonb NOT NULL DEFAULT '{}',
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sending', 'done', 'failed')),
    attempts integer NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL,
    last_error text,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE INDEX ledger_jobs_due ON ledger_jobs (org_id, next_attempt_at) WHERE status IN ('pending', 'failed');
CREATE UNIQUE INDEX ledger_jobs_one_live ON ledger_jobs (org_id, kind, bill_id, payment_id)
    WHERE status IN ('pending', 'sending');
ALTER TABLE ledger_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE ledger_jobs FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON ledger_jobs
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

-- The bank reference last sent to the ledger for a payment link, so a
-- poll that changes nothing sends nothing.
ALTER TABLE ledger_links ADD COLUMN bank_reference text;
