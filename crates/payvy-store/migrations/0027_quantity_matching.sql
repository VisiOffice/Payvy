-- Quantity matching (PO Matching Spec v2, roadmap 1): bill lines carry the
-- quantity, unit and unit price as printed, so ordered vs billed can be
-- checked cumulatively per PO line. Quantities are exact micro-units
-- (1 = 1_000_000; payvy-domain::invoice_evidence::quantity_micro) — no
-- float ever enters quantity arithmetic. Absent means amount-only: the
-- line is stored and displayed but not quantity-checked.
ALTER TABLE bill_lines ADD COLUMN quantity_micro bigint;
ALTER TABLE bill_lines ADD COLUMN unit text;
ALTER TABLE bill_lines ADD COLUMN unit_price_cents bigint;

-- "Bills allocated before this one" needs an allocation order that
-- timestamps can't give (two allocations can share one); a sequence can.
-- Existing rows are numbered in creation order explicitly: an identity
-- column added in one step would number them in heap order, and rows
-- updated since insert (reserved -> consumed) can sit anywhere in it.
ALTER TABLE po_allocations ADD COLUMN seq bigint;
-- Forced RLS applies to migrations too: without lifting it, the UPDATE
-- sees no rows in any organization and the NOT NULL below then fails on
-- any database that already has allocations (0018/0020/0022 pattern).
ALTER TABLE po_allocations NO FORCE ROW LEVEL SECURITY;
UPDATE po_allocations SET seq = numbered.n
FROM (SELECT id, row_number() OVER (ORDER BY created_at, id) AS n FROM po_allocations) numbered
WHERE po_allocations.id = numbered.id;
ALTER TABLE po_allocations ALTER COLUMN seq SET NOT NULL;
ALTER TABLE po_allocations ALTER COLUMN seq ADD GENERATED ALWAYS AS IDENTITY;
-- Still under NO FORCE: with it restored, MAX(seq) would see no rows.
SELECT setval(pg_get_serial_sequence('po_allocations', 'seq'),
              COALESCE((SELECT MAX(seq) FROM po_allocations), 0) + 1, false);
ALTER TABLE po_allocations FORCE ROW LEVEL SECURITY;

-- The registry's second policy: quantity_matching is flag_only (findings
-- show beside the bill, never block) or enforce (quantity_over and
-- rate_mismatch block payment like other exceptions). Existing versions
-- take the default.
ALTER TABLE vendor_po_policies ADD COLUMN quantity_matching text NOT NULL
    DEFAULT 'flag_only' CHECK (quantity_matching IN ('flag_only', 'enforce'));
