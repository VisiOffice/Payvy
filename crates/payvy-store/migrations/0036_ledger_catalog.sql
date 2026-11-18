-- The ledger catalog: ledger.sync reads the books' accounts, periods,
-- vendors and segments into ledger_records so a bill is checked before it
-- is sent. VisiBooks segments are one list of typed segments, so they get
-- their own kind instead of department/class/location. A row that leaves
-- the books is marked inactive, never deleted: links may still name it.
ALTER TABLE ledger_records DROP CONSTRAINT ledger_records_kind_check;
ALTER TABLE ledger_records ADD CONSTRAINT ledger_records_kind_check
    CHECK (kind IN ('vendor', 'account', 'department', 'class', 'location', 'period', 'segment'));

-- When the catalog was last read in full. Null: never.
ALTER TABLE ledger_connections ADD COLUMN catalog_synced_at timestamptz;
