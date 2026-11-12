-- Quantity matching (PO Matching Spec v2, roadmap 1): bill lines carry the
-- quantity, unit and unit price as printed, so ordered vs billed can be
-- checked cumulatively per PO line. Quantities are exact micro-units
-- (1 = 1_000_000; payvy-domain::invoice_evidence::quantity_micro) — no
-- float ever enters quantity arithmetic. Absent means amount-only: the
-- line is stored and displayed but not quantity-checked.
ALTER TABLE bill_lines ADD COLUMN quantity_micro bigint;
ALTER TABLE bill_lines ADD COLUMN unit text;
ALTER TABLE bill_lines ADD COLUMN unit_price_cents bigint;

-- The registry's second policy: quantity_matching is flag_only (findings
-- show beside the bill, never block) or enforce (quantity_over and
-- rate_mismatch block payment like other exceptions). Existing versions
-- take the default.
ALTER TABLE vendor_po_policies ADD COLUMN quantity_matching text NOT NULL
    DEFAULT 'flag_only' CHECK (quantity_matching IN ('flag_only', 'enforce'));
