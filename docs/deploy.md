# Hosted deploy (api.payvy.com)

Hosted Payvy runs on `thatch-api-prod` (5.78.94.150, Hetzner Hillsboro) with
[Kamal](https://kamal-deploy.org), beside VisiStaff and Unicorn Phonics. Self-hosters can use
this as a template; nothing in the server depends on it.

```bash
kamal deploy          # build, push to ghcr.io/regatlas-llc/payvy and roll out
kamal app logs -f
kamal accessory boot db   # first time only
```

Kamal runs under the mise Ruby; if it reports `can't find gem kamal`:
`PATH=~/.local/share/mise/installs/ruby/3.4.11/bin:$PATH kamal deploy`.

## How it runs

- **App:** the `app` target of the `Dockerfile`, read-only with no capabilities. It applies
  migrations at startup.
- **Database:** the `payvy-db` accessory (Postgres 18.6), data in `/root/payvy-db/data`. The
  init script `deploy/postgres-init/01-app-role.sh` creates the `payvy` role, which owns the
  database but is not a superuser, so row-level security holds in production.
- **Traffic:** Cloudflare DNS (`api.payvy.com`, A record, not proxied) → host nginx, TLS from
  certbot (`deploy/nginx-api.payvy.com.conf`) → kamal-proxy on `127.0.0.1:8090` → the app on
  7430. kamal-proxy is shared: its `proxy.run` settings must match the other apps on the box.
- **Documents:** R2 bucket `payvy-documents` in the Robert@payvy.com Cloudflare account.
- **Inbound email:** `in.payvy.com` on the payvy.com Email Routing. The zone's catch-all still
  belongs to the legacy app's `payvy-email-worker`, which hands `@in.payvy.com` mail to
  `/webhooks/email`. When the legacy app is retired, the catch-all moves to
  `workers/email-inbound`.
- **Secrets:** `.kamal/secrets` (not committed; see `.kamal/secrets.example`) fetches every
  value from 1Password through VisiPass, one approval per deploy.

## First-time setup

1. Create the secrets in 1Password without seeing them (each `openssl rand -hex 32 | vp create
   api-credential --stdin --title ... --vault ...`): Postgres superuser password, app role
   password, `PAYVY_API_KEY`, `PAYVY_INBOUND_SECRET`. Add an R2 API token scoped to
   `payvy-documents` (Object Read & Write) as two items, and an Anthropic API key for reading
   documents (`PAYVY_EXTRACT_API_KEY`). Put the references in `.kamal/secrets`.
2. Cloudflare: A record `api.payvy.com` → 5.78.94.150 (DNS only); Email Routing → add the
   subdomain `in.payvy.com`.
3. On the host: copy the nginx file to `/etc/nginx/sites-available/payvy`, link it, get the
   certificate (`certbot certonly --webroot -w /var/www/payvy-acme -d api.payvy.com`), then
   `nginx -t && systemctl reload nginx`.
4. `kamal accessory boot db`, then `kamal deploy`.
5. Legacy worker: `wrangler secret put PAYVY_INBOUND_SECRET` and deploy it.
6. Create the first person key (`kamal app exec "payvy key create --actor robert --kind person"`)
   and the inbound address (`email.new_address`), then send a test bill.
