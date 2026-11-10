#!/usr/bin/env bash
# One-time host setup for hosted Payvy on thatch-api-prod: nginx for dev.payvy.com and
# beta.payvy.com, and their certificate. Safe to run again. Run from the repo root:
#
#   H="-J root@100.95.75.126 root@5.78.94.150"
#   ssh $H 'cat > /tmp/payvy-nginx.conf' < deploy/nginx-payvy-beta.conf
#   ssh $H 'bash -s' < deploy/setup-host.sh
set -euo pipefail

SITE=/etc/nginx/sites-available/payvy
CERT=/etc/letsencrypt/live/dev.payvy.com/fullchain.pem

command -v nginx >/dev/null || { echo "nginx isn't installed here" >&2; exit 1; }
command -v certbot >/dev/null || { echo "certbot isn't installed here" >&2; exit 1; }
[ -f /etc/letsencrypt/options-ssl-nginx.conf ] || {
  echo "/etc/letsencrypt/options-ssl-nginx.conf is missing (install python3-certbot-nginx)" >&2
  exit 1
}
mkdir -p /var/www/payvy-acme

if [ ! -f "$CERT" ]; then
  # Port 80 only, so certbot can answer the challenge before the certificate exists.
  cat >"$SITE" <<'EOF'
server {
    listen 80;
    server_name dev.payvy.com beta.payvy.com;
    location /.well-known/acme-challenge/ { root /var/www/payvy-acme; }
    location / { return 404; }
}
EOF
  ln -sf "$SITE" /etc/nginx/sites-enabled/payvy
  nginx -t && systemctl reload nginx
  certbot certonly --webroot -w /var/www/payvy-acme -d dev.payvy.com -d beta.payvy.com \
    --non-interactive --agree-tos --keep-until-expiring
fi

[ -s /tmp/payvy-nginx.conf ] || {
  echo "send deploy/nginx-payvy-beta.conf to /tmp/payvy-nginx.conf first" >&2
  exit 1
}
install -m 644 /tmp/payvy-nginx.conf "$SITE"
ln -sf "$SITE" /etc/nginx/sites-enabled/payvy
nginx -t && systemctl reload nginx
echo "nginx serves dev.payvy.com and beta.payvy.com with $(openssl x509 -noout -enddate -in "$CERT")"
