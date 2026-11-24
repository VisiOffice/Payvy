-- What the invoice printed that ties it to a purchase order, kept with the
-- bill so a PO that arrives later can still find it (automatic matching).
-- Both as read or entered; NULL for every bill before this.
ALTER TABLE bills ADD COLUMN po_reference text;
ALTER TABLE bills ADD COLUMN service_address text;
