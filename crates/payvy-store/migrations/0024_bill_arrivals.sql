-- The invoice stack: every copy of an invoice a vendor sends for one bill.
-- The first arrival creates the bill; a later copy with the same vendor +
-- normalized invoice number (bills.invoice_key) stacks here instead of
-- becoming a second payable bill. A copy carrying a different amount is a
-- revision: decision 'pending' until a person accepts (the bill takes its
-- amount and lines) or rejects it (the bill keeps its amount). Capacity,
-- matching and payment always see the bill's one current amount.
CREATE TABLE bill_arrivals (
    id uuid PRIMARY KEY,
    -- Stack order: copies can share a received_at, so arrival order is a
    -- sequence, never the timestamp or the random id.
    seq bigint GENERATED ALWAYS AS IDENTITY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    bill_id uuid NOT NULL,
    document_id uuid,
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    invoice_number text NOT NULL,
    lines jsonb NOT NULL DEFAULT '[]',
    received_by text NOT NULL,
    received_at timestamptz NOT NULL,
    decision text NOT NULL DEFAULT 'none'
        CHECK (decision IN ('none', 'pending', 'accepted', 'rejected', 'superseded')),
    decided_by text,
    decided_at timestamptz
);
CREATE INDEX bill_arrivals_bill ON bill_arrivals (bill_id, seq);
ALTER TABLE bill_arrivals ENABLE ROW LEVEL SECURITY;
ALTER TABLE bill_arrivals FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON bill_arrivals
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
ALTER TABLE bill_arrivals ADD CONSTRAINT bill_arrivals_bill_org
    FOREIGN KEY (org_id, bill_id) REFERENCES bills (org_id, id);
ALTER TABLE bill_arrivals ADD CONSTRAINT bill_arrivals_document_org
    FOREIGN KEY (org_id, document_id) REFERENCES documents (org_id, id);

-- Bills that already exist have been received once. The backfill reads
-- bills, documents and bill_lines before any org is selected, so their
-- forced RLS is lifted for it (the pattern 0020 uses for its constraints).
ALTER TABLE bills NO FORCE ROW LEVEL SECURITY;
ALTER TABLE documents NO FORCE ROW LEVEL SECURITY;
ALTER TABLE bill_lines NO FORCE ROW LEVEL SECURITY;
ALTER TABLE bill_arrivals NO FORCE ROW LEVEL SECURITY;
INSERT INTO bill_arrivals
    (id, org_id, bill_id, document_id, amount_cents, invoice_number, lines,
     received_by, received_at, decision)
SELECT gen_random_uuid(), b.org_id, b.id,
       (SELECT d.id FROM documents d
         WHERE d.bill_id = b.id ORDER BY d.received_at, d.id LIMIT 1),
       b.amount_cents, b.invoice_number,
       COALESCE((SELECT jsonb_agg(
                     jsonb_build_object('description', l.description,
                                        'amount_cents', l.amount_cents,
                                        'account', l.account)
                     ORDER BY l.position)
                 FROM bill_lines l WHERE l.bill_id = b.id), '[]'::jsonb),
       b.created_by, b.received_at, 'none'
FROM bills b ORDER BY b.received_at, b.id;
ALTER TABLE bills FORCE ROW LEVEL SECURITY;
ALTER TABLE documents FORCE ROW LEVEL SECURITY;
ALTER TABLE bill_lines FORCE ROW LEVEL SECURITY;
ALTER TABLE bill_arrivals FORCE ROW LEVEL SECURITY;
