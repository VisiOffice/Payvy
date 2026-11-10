#!/bin/bash
# The app connects as a plain role that owns its database. It is not a
# superuser and can't bypass row-level security, so organization isolation is
# enforced in production exactly as in development.
set -euo pipefail
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" -v pw="$PAYVY_DB_PASSWORD" <<'SQL'
CREATE ROLE payvy LOGIN PASSWORD :'pw' NOSUPERUSER NOBYPASSRLS;
ALTER DATABASE payvy OWNER TO payvy;
SQL
