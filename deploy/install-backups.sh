#!/usr/bin/env bash
# Installs the nightly database backup on the host, from your machine. Run from the repo root:
#
#   deploy/install-backups.sh
#
# Copies backup-db.sh and restore-check.sh to /root/bin on the host, writes
# /root/payvy-backup.env (mode 600) with the B2 key and the encryption key from
# 1Password through VisiPass (one approval; the values never touch this disk),
# adds the cron line (09:17 UTC, after midnight in the US), then runs one backup
# and a restore check. Safe to run again.
set -euo pipefail

HOST=root@5.78.94.150
JUMP=root@100.95.75.126
R=op://z4qnxfjveoupts3ll25tct3l2v
ssh_h() { ssh -J "$JUMP" "$HOST" "$@"; }

ssh_h 'install -d -m 700 /root/bin'
scp -q -J "$JUMP" deploy/backup-db.sh deploy/restore-check.sh "$HOST:/root/bin/"
ssh_h 'chmod 700 /root/bin/backup-db.sh /root/bin/restore-check.sh'

vp run --purpose "Install the Payvy database backup credentials on the host" \
  --env B2_KEY_ID="$R/an4i7nhm3zwocbqx7ubyl2ucvj/credential" \
  --env B2_APPLICATION_KEY="$R/mizzlos64lf5yotnn47pifoevu/credential" \
  --env BACKUP_ENCRYPTION_KEY="$R/xwruixjq7xoxp2hoxmgpjynqnm/credential" \
  -- bash -c 'printf "B2_KEY_ID=%s\nB2_APPLICATION_KEY=%s\nBACKUP_ENCRYPTION_KEY=%s\nB2_BUCKET=payvy-db-backups\n" \
       "$(printf %s "$B2_KEY_ID" | tr -d "[:space:]")" \
       "$(printf %s "$B2_APPLICATION_KEY" | tr -d "[:space:]")" \
       "$BACKUP_ENCRYPTION_KEY" \
     | ssh -J '"$JUMP $HOST"' "umask 077; cat > /root/payvy-backup.env"'

ssh_h '(crontab -l 2>/dev/null | grep -v "# payvy-backup$" || true;
        echo "17 9 * * * /root/bin/backup-db.sh >> /var/log/payvy-backup.log 2>&1 # payvy-backup") | crontab -
       crontab -l | grep payvy-backup'

echo "--- first backup and restore check ---"
ssh_h '/root/bin/backup-db.sh && /root/bin/restore-check.sh'
