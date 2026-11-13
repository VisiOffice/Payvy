-- Where a ledger connection actually lives. #41/#45 stored the provider,
-- entity and sealed key, but a VisiBooks connection also needs its HTTPS
-- origin (Phase 1 of the Allied cutover: the app builds the adapter from
-- the row, per company, instead of a server-wide adapter). Existing rows
-- (NetSuite-shaped, tests) leave it null; a VisiBooks connection refuses
-- to post without it, with a named error.
ALTER TABLE ledger_connections ADD COLUMN origin text;
