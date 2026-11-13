#!/usr/bin/env bash
# Nightly backup of the hosted Payvy database to Backblaze B2. Runs on the host
# (thatch-api-prod) from root's crontab; see docs/deploy.md, "Database backups".
#
# - Dumps as the Postgres superuser inside the payvy-db accessory, so row-level
#   security doesn't hide any company's rows (the app role would dump only what
#   it can see).
# - Encrypts with BACKUP_ENCRYPTION_KEY (openssl AES-256-CBC, PBKDF2) before it
#   leaves the host, then uploads to the payvy-db-backups bucket under
#   backups/{daily,weekly,monthly}/. The bucket's lifecycle rules expire them
#   (30 days, 90 days, a year); this script never deletes.
# - Reads its credentials from /root/payvy-backup.env (mode 600):
#     B2_KEY_ID, B2_APPLICATION_KEY, BACKUP_ENCRYPTION_KEY
#   and optionally B2_BUCKET (default payvy-db-backups) and HEARTBEAT_URL,
#   pinged on success and with /fail on failure.
# - No secret is ever passed on a command line (visible in ps): curl reads its
#   credentials from a config on stdin, openssl from the environment.
set -euo pipefail

ENV_FILE=${PAYVY_BACKUP_ENV:-/root/payvy-backup.env}
DB_CONTAINER=${PAYVY_DB_CONTAINER:-payvy-db}
DB_NAME=${PAYVY_DB_NAME:-payvy}
say() { echo "[payvy-backup] $(date -u +%FT%TZ) $*"; }

# shellcheck disable=SC1090
if [ -f "$ENV_FILE" ]; then set -a; . "$ENV_FILE"; set +a; fi
PREFIX=${B2_PREFIX:-backups}
B2_BUCKET=${B2_BUCKET:-payvy-db-backups}
# A pasted key often carries a trailing newline or space; B2 then answers 401.
B2_KEY_ID=$(printf %s "${B2_KEY_ID:-}" | tr -d '[:space:]')
B2_APPLICATION_KEY=$(printf %s "${B2_APPLICATION_KEY:-}" | tr -d '[:space:]')
for v in B2_KEY_ID B2_APPLICATION_KEY BACKUP_ENCRYPTION_KEY; do
  [ -n "${!v:-}" ] || { say "ERROR: $v is not set in $ENV_FILE"; exit 1; }
done

fail() {
  say "ERROR: $*"
  [ -n "${HEARTBEAT_URL:-}" ] && curl -fsS -m 10 --retry 3 "$HEARTBEAT_URL/fail" >/dev/null || true
  exit 1
}
trap 'fail "line $LINENO failed"' ERR

STAMP=$(date -u +%Y%m%dT%H%M%SZ)
if [ "$(date -u +%d)" = 01 ]; then KIND=monthly
elif [ "$(date -u +%u)" = 7 ]; then KIND=weekly
else KIND=daily; fi
NAME="${PREFIX}/${KIND}/payvy-${KIND}-${STAMP}.dump.enc"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
OUT="$TMP/payvy.dump.enc"

say "dumping $DB_NAME from $DB_CONTAINER ($KIND)"
# Custom format is compressed and restores with pg_restore, table by table.
docker exec "$DB_CONTAINER" pg_dump -U postgres -Fc "$DB_NAME" \
  | openssl enc -aes-256-cbc -salt -pbkdf2 -iter 200000 -pass env:BACKUP_ENCRYPTION_KEY > "$OUT"
SIZE=$(stat -c%s "$OUT")
[ "$SIZE" -gt 1024 ] || fail "dump is only $SIZE bytes"
say "dump is $SIZE bytes encrypted"

# B2 native API. Credentials go to curl on stdin, never on its command line.
AUTH=$(printf 'user = "%s:%s"\n' "$B2_KEY_ID" "$B2_APPLICATION_KEY" \
  | curl -fsS -m 30 -K - https://api.backblazeb2.com/b2api/v3/b2_authorize_account)
field() { sed -n "s/.*\"$1\": *\"\\([^\"]*\\)\".*/\\1/p" | head -1; }
TOKEN=$(printf '%s' "$AUTH" | tr -d '\n' | field authorizationToken)
API=$(printf '%s' "$AUTH" | tr -d '\n' | grep -o '"apiUrl": *"[^"]*"' | head -1 | cut -d'"' -f4)
ACCOUNT=$(printf '%s' "$AUTH" | tr -d '\n' | field accountId)
[ -n "$TOKEN" ] && [ -n "$API" ] && [ -n "$ACCOUNT" ] || fail "B2 sign-in failed"

b2() { # b2 <call> <json>: POST with the account token, from stdin config
  printf 'header = "Authorization: %s"\n' "$TOKEN" \
    | curl -fsS -m 60 -K - -d "$2" "$API/b2api/v3/$1"
}
BUCKET_ID=$(b2 b2_list_buckets "{\"accountId\":\"$ACCOUNT\",\"bucketName\":\"$B2_BUCKET\"}" \
  | tr -d '\n' | field bucketId)
[ -n "$BUCKET_ID" ] || fail "bucket $B2_BUCKET not found"
UP=$(b2 b2_get_upload_url "{\"bucketId\":\"$BUCKET_ID\"}" | tr -d '\n')
UP_URL=$(printf '%s' "$UP" | field uploadUrl)
UP_TOKEN=$(printf '%s' "$UP" | field authorizationToken)
[ -n "$UP_URL" ] && [ -n "$UP_TOKEN" ] || fail "no upload URL"

SHA1=$(sha1sum "$OUT" | cut -d' ' -f1)
printf 'header = "Authorization: %s"\n' "$UP_TOKEN" \
  | curl -fsS -m 1800 -K - -o /dev/null \
      -H "X-Bz-File-Name: $NAME" -H "Content-Type: application/octet-stream" \
      -H "X-Bz-Content-Sha1: $SHA1" --data-binary "@$OUT" "$UP_URL" \
  || fail "upload of $NAME failed"
say "uploaded $NAME ($SIZE bytes)"

[ -n "${HEARTBEAT_URL:-}" ] && curl -fsS -m 10 --retry 3 "$HEARTBEAT_URL" >/dev/null || true
