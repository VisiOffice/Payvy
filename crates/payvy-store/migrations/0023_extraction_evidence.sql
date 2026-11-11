-- Immutable extraction analysis: schema/prompt provenance, coverage and source evidence.
ALTER TABLE extractions ADD COLUMN analysis jsonb;
ALTER TABLE invoice_review_settings ADD COLUMN extraction_v2_enabled boolean NOT NULL DEFAULT false;

-- Persist enrollment before external reading; disabling during a read cannot bypass review.
ALTER TABLE documents ADD COLUMN extraction_v2_enrolled boolean NOT NULL DEFAULT false;

-- A bounded lease prevents duplicate model calls without holding a business lock.
ALTER TABLE documents ADD COLUMN extraction_read_token uuid;
ALTER TABLE documents ADD COLUMN extraction_read_started_at timestamptz;
