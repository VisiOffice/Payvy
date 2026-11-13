#!/usr/bin/env bash
# Proves the newest backup in B2 restores: downloads it, decrypts it, restores it
# into a scratch database inside payvy-db, counts rows in a few tables next to
# the live database, then drops the scratch database. Read-only for the live
# database. Run on the host as root, with the same /root/payvy-backup.env as
# backup-db.sh. Run it after setting backups up, and now and then after that.
set -euo pipefail

ENV_FILE=${PAYVY_BACKUP_ENV:-/root/payvy-backup.env}
DB_CONTAINER=${PAYVY_DB_CONTAINER:-payvy-db}
DB_NAME=${PAYVY_DB_NAME:-payvy}
SCRATCH=payvy_restore_check
say() { echo "[payvy-restore-check] $*"; }

# shellcheck disable=SC1090
if [ -f "$ENV_FILE" ]; then set -a; . "$ENV_FILE"; set +a; fi
PREFIX=${B2_PREFIX:-backups}
B2_BUCKET=${B2_BUCKET:-payvy-db-backups}
# A pasted key often carries a trailing newline or space; B2 then answers 401.
B2_KEY_ID=$(printf %s "${B2_KEY_ID:-}" | tr -d '[:space:]')
B2_APPLICATION_KEY=$(printf %s "${B2_APPLICATION_KEY:-}" | tr -d '[:space:]')

AUTH=$(printf 'user = "%s:%s"\n' "$B2_KEY_ID" "$B2_APPLICATION_KEY" \
  | curl -fsS -m 30 -K - https://api.backblazeb2.com/b2api/v3/b2_authorize_account | tr -d '\n')
field() { sed -n "s/.*\"$1\": *\"\\([^\"]*\\)\".*/\\1/p" | head -1; }
TOKEN=$(printf '%s' "$AUTH" | field authorizationToken)
API=$(printf '%s' "$AUTH" | grep -o '"apiUrl": *"[^"]*"' | head -1 | cut -d'"' -f4)
DOWN=$(printf '%s' "$AUTH" | grep -o '"downloadUrl": *"[^"]*"' | head -1 | cut -d'"' -f4)
ACCOUNT=$(printf '%s' "$AUTH" | field accountId)
b2() {
  printf 'header = "Authorization: %s"\n' "$TOKEN" \
    | curl -fsS -m 60 -K - -d "$2" "$API/b2api/v3/$1"
}
BUCKET_ID=$(b2 b2_list_buckets "{\"accountId\":\"$ACCOUNT\",\"bucketName\":\"$B2_BUCKET\"}" \
  | tr -d '\n' | field bucketId)

# Newest across daily/weekly/monthly: names sort by their UTC timestamp.
LATEST=$(b2 b2_list_file_names "{\"bucketId\":\"$BUCKET_ID\",\"prefix\":\"$PREFIX/\",\"maxFileCount\":10000}" \
  | grep -o '"fileName": *"[^"]*"' | cut -d'"' -f4 \
  | awk -F- '{print $NF" "$0}' | sort | tail -1 | cut -d' ' -f2)
[ -n "$LATEST" ] || { say "no backups in $B2_BUCKET"; exit 1; }
say "newest backup: $LATEST"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"; docker exec "$DB_CONTAINER" dropdb -U postgres --if-exists "$SCRATCH" >/dev/null 2>&1 || true' EXIT
printf 'header = "Authorization: %s"\n' "$TOKEN" \
  | curl -fsS -m 1800 -K - -o "$TMP/b.enc" "$DOWN/file/$B2_BUCKET/$LATEST"
openssl enc -d -aes-256-cbc -pbkdf2 -iter 200000 -pass env:BACKUP_ENCRYPTION_KEY \
  -in "$TMP/b.enc" -out "$TMP/b.dump"
say "decrypted $(stat -c%s "$TMP/b.dump") bytes"

docker exec "$DB_CONTAINER" dropdb -U postgres --if-exists "$SCRATCH"
docker exec "$DB_CONTAINER" createdb -U postgres "$SCRATCH"
docker exec -i "$DB_CONTAINER" pg_restore -U postgres --no-owner --exit-on-error -d "$SCRATCH" < "$TMP/b.dump"

count() { docker exec "$DB_CONTAINER" psql -U postgres -d "$1" -Atc "$2"; }
Q_TABLES="select count(*) from information_schema.tables where table_schema='public'"
printf '%-22s %10s %10s\n' "" backup live
printf '%-22s %10s %10s\n' tables "$(count "$SCRATCH" "$Q_TABLES")" "$(count "$DB_NAME" "$Q_TABLES")"
for t in organizations people vendors bills documents payments events; do
  q="select count(*) from $t"
  printf '%-22s %10s %10s\n' "$t" "$(count "$SCRATCH" "$q" 2>/dev/null || echo -)" "$(count "$DB_NAME" "$q" 2>/dev/null || echo -)"
done
say "restore OK (live may be slightly ahead of the backup)"
