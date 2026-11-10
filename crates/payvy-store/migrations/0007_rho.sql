-- Rho: a second bank connector that can only read. Its API (Rho API v1) is
-- read-only for accounts and transactions, so a Rho connection can coexist
-- with the paying connection: it never pays anyone, it reports balances and
-- transactions. The one-live-connection rule becomes one live *paying*
-- connection; read-only connections don't count against it.

ALTER TABLE bank_connections DROP CONSTRAINT bank_connections_provider_check;
ALTER TABLE bank_connections ADD CONSTRAINT bank_connections_provider_check
    CHECK (provider IN ('mercury', 'rho'));

ALTER TABLE bank_connections DROP CONSTRAINT bank_connections_mode_check;
ALTER TABLE bank_connections ADD CONSTRAINT bank_connections_mode_check
    CHECK (mode IN ('request', 'direct', 'read_only'));

DROP INDEX bank_connections_one_live;
CREATE UNIQUE INDEX bank_connections_one_live_paying ON bank_connections (org_id)
    WHERE retired_at IS NULL AND mode <> 'read_only';
CREATE UNIQUE INDEX bank_connections_one_live_readonly ON bank_connections (org_id, provider)
    WHERE retired_at IS NULL AND mode = 'read_only';
