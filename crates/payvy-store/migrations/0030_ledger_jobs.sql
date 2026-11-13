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
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sending', 'done', 'failed', 'dead')),
    attempts integer NOT NULL DEFAULT 0,
    lease_until timestamptz,
    next_attempt_at timestamptz NOT NULL,
    last_error text,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
CREATE INDEX ledger_jobs_due ON ledger_jobs (org_id, next_attempt_at) WHERE status IN ('pending', 'failed');
-- NULLS NOT DISTINCT so bill jobs (payment_id NULL) dedupe too.
CREATE UNIQUE INDEX ledger_jobs_one_live ON ledger_jobs (org_id, kind, bill_id, payment_id)
    NULLS NOT DISTINCT WHERE status IN ('pending', 'sending');
-- The foreign-key validation scan reads bills under forced RLS, whose
-- policy needs the per-request org setting migrations don't have.
ALTER TABLE bills NO FORCE ROW LEVEL SECURITY;
ALTER TABLE ledger_jobs ADD CONSTRAINT ledger_jobs_bill_org
    FOREIGN KEY (org_id, bill_id) REFERENCES bills (org_id, id);
ALTER TABLE bills FORCE ROW LEVEL SECURITY;
-- payment_id gets no foreign key on purpose: an outside payment's job
-- keys its payment leg by the bill id (there is no payments row).
ALTER TABLE ledger_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE ledger_jobs FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON ledger_jobs
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);

-- The bank reference last sent to the ledger for a payment link, so a
-- poll that changes nothing sends nothing.
ALTER TABLE ledger_links ADD COLUMN bank_reference text;
