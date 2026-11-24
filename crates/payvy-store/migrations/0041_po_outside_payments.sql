-- Declarations that a purchase order (or one of its invoices) was paid
-- outside Payvy — by card by phone, check, cash, etc. (docs/purchase-orders.md).
-- A declaration blocks payment of the bill it concerns right away, and marks
-- the bill paid once effective (confirmed, or accepted from the PO source
-- under policy) and released by a person.
CREATE TABLE po_outside_payments (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    po_id uuid NOT NULL REFERENCES purchase_orders(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    method text NOT NULL CHECK (method IN ('card', 'check', 'ach', 'wire', 'cash', 'other')),
    paid_on date NOT NULL,
    reference text CHECK (reference IS NULL OR char_length(reference) <= 64),
    note text NOT NULL,
    invoice_number text,
    declared_by text NOT NULL,
    declared_by_source boolean NOT NULL DEFAULT false,
    declared_at timestamptz NOT NULL,
    status text NOT NULL CHECK (status IN ('declared', 'confirmed', 'withdrawn')),
    confirmed_by text,
    confirmed_at timestamptz,
    withdrawn_by text,
    withdrawn_at timestamptz,
    withdrawn_reason text,
    bill_id uuid REFERENCES bills(id),
    CHECK (status <> 'withdrawn' OR bill_id IS NULL),
    CHECK (status <> 'confirmed' OR (confirmed_by IS NOT NULL AND confirmed_at IS NOT NULL)),
    CHECK (status <> 'withdrawn' OR (withdrawn_by IS NOT NULL AND withdrawn_at IS NOT NULL AND withdrawn_reason IS NOT NULL))
);

-- A declaration settles at most one whole bill, and a bill is settled by at
-- most one declaration.
CREATE UNIQUE INDEX po_outside_payments_one_per_bill
    ON po_outside_payments (org_id, bill_id) WHERE bill_id IS NOT NULL;
CREATE INDEX po_outside_payments_by_po
    ON po_outside_payments (org_id, po_id, declared_at);
CREATE INDEX po_outside_payments_live_by_vendor
    ON po_outside_payments (org_id, vendor_id) WHERE status <> 'withdrawn' AND bill_id IS NULL;

ALTER TABLE po_outside_payments ENABLE ROW LEVEL SECURITY;
ALTER TABLE po_outside_payments FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON po_outside_payments
    USING (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid)
    WITH CHECK (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid);

-- The registry's third policy: paid_outside is person_confirms (default: a
-- declaration is effective only once confirmed by someone else) or
-- accept_from_source (a declaration made by the standing-authorization source
-- key is effective at once; anyone else's still needs confirming). Existing
-- versions take the default.
ALTER TABLE vendor_po_policies ADD COLUMN paid_outside text NOT NULL
    DEFAULT 'person_confirms' CHECK (paid_outside IN ('person_confirms', 'accept_from_source'));
