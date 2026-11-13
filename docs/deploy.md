# Hosted deploy (dev.payvy.com)

**Cut over 2026-11-12:** the web app is `app.payvy.com` (and still `beta.payvy.com`); the API,
MCP and webhooks stay at `dev.payvy.com` (Mercury's webhooks are registered there). The legacy
app moved to `old.payvy.com` (Worker `payvy-fe`, backend `be.payvy.com`) for looking up old
records.

The frontend is the `payvy-web` Cloudflare Worker at `app.payvy.com` and `beta.payvy.com`.
The API runs on `thatch-api-prod` (5.78.94.150, Hetzner Hillsboro) with
[Kamal](https://kamal-deploy.org), beside VisiStaff and Unicorn Phonics. Self-hosters can use
this as a template; nothing in the server depends on it.

```bash
kamal deploy          # build, push to ghcr.io/regatlas-llc/payvy and roll out
kamal app logs -f
kamal accessory boot db   # first time only
```

Kamal runs under the mise Ruby; if it reports `can't find gem kamal`:
`PATH=~/.local/share/mise/installs/ruby/3.4.11/bin:$PATH kamal deploy`.

## Frontend: Cloudflare Workers Builds

`web/wrangler.jsonc` serves `web/dist` as a single-page app and sends only `/api/*`
and `/auth/*` to `https://dev.payvy.com`. Session cookies stay on the host the app was opened on;
the Worker streams requests and responses, disables API caching, and never retries
commands. Rust still owns authentication, authorization, previews and confirmation.
Keep `PAYVY_PUBLIC_URL=https://app.payvy.com` on the API so emailed links open the frontend.

Connect **VisiOffice/Payvy** to **payvy-web** in the Robert@payvy.com Cloudflare
account, under **Settings → Builds → Connect**, with these settings:

| Setting | Value |
| --- | --- |
| Production branch | `main` |
| Root directory | `/` (the pnpm workspace root) |
| Build command | `pnpm --dir web build` |
| Deploy command | `pnpm --dir web deploy` |
| Build variable | `NODE_VERSION=22.17.0` |
| Build variable | `PNPM_VERSION=10.12.4` |
| Include paths | `web/*`, `package.json`, `pnpm-lock.yaml`, `pnpm-workspace.yaml` |
| Preview/non-production branch builds | Disabled |

Use Cloudflare's dependency installation with the committed pnpm lockfile. No Rust
toolchain, backend secrets, bank credentials or GitHub Actions workflow is needed.
The account's existing Cloudflare GitHub installation must have access to this repo.
The Git connection and build watch paths are dashboard settings, not Wrangler settings.

Branch previews are disabled because they would otherwise use the live API and sign-in
links. Configure a separate API and public URL before enabling authenticated previews.

Local verification and manual deployment:

```bash
pnpm --dir web build
pnpm --dir web deploy:check
PAYVY_E2E_EDGE_PORT=7434 pnpm --dir web e2e  # full suite through local Cloudflare runtime
pnpm --dir web deploy                       # bootstrap or manual recovery
```

Cloudflare owns the `app.payvy.com` and `beta.payvy.com` custom domains after deploying the
Worker. A custom domain belongs to one Worker: `app.payvy.com` had to be removed from the
legacy `payvy-fe` Worker before `payvy-web` could take it. The Docker image still
includes the frontend for self-hosters and rollback. Hosted frontend-only changes
need only a GitHub push after the Builds connection is enabled; API changes still
need `kamal deploy`. Deploy compatible API additions before frontend code that uses them.

For the initial cutover, Cloudflare may reject the custom domain with error `100117`
while beta's existing DNS-only A record exists. Record its value, remove only the
`beta.payvy.com` A record, then immediately attach `beta.payvy.com` under the Worker's
**Settings → Domains & Routes → Add → Custom Domain** (or rerun `pnpm --dir web deploy`).
Do not remove `dev.payvy.com`, which the proxy still needs.

After cutover, update the origin's Certbot certificate to cover only `dev.payvy.com`
(`certbot certonly --webroot -w /var/www/payvy-acme --cert-name dev.payvy.com -d dev.payvy.com`,
then `nginx -t && systemctl reload nginx`). Its existing certificate also covers beta;
leaving beta in that certificate would make future HTTP-01 renewals contact the Worker
instead of the origin's challenge directory. Cloudflare manages beta's new certificate.

To roll back a frontend release, use the Worker's deployment history. To move beta
back to Hetzner, remove its Worker custom domain and restore its DNS-only A record
to `5.78.94.150`; the existing nginx/Kamal host configuration still serves it.

References: [Workers monorepos](https://developers.cloudflare.com/workers/ci-cd/builds/advanced-setups/),
[build watch paths](https://developers.cloudflare.com/workers/ci-cd/builds/build-watch-paths/).

## CI (GitHub Actions)

`.github/workflows/ci.yml` runs on every push and pull request. It checks; it never deploys
and uses no repository secrets.

- **Rust:** `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` and
  `cargo test --workspace`, with `.env.example` loaded and the stores from `docker-compose.yml`
  (Postgres with the non-superuser `payvy` role, so RLS is enforced; SeaweedFS for S3). The job
  fails if any database or S3 test printed `skipping:`, so tests can't pass by not running.
- **Web:** `pnpm install --frozen-lockfile`, `pnpm --dir web gen:api` must leave
  `web/src/api` unchanged, `pnpm -r check`, then the full Playwright suite (`pnpm --dir web e2e`)
  through the Worker in local workerd (`PAYVY_E2E_EDGE_PORT`), against the same stores and the
  fake model and banks. On failure the HTML report, traces and
  the server log are uploaded as the `playwright-report` artifact.

## How the API runs

- **App:** the `app` target of the `Dockerfile`, read-only with no capabilities. It applies
  migrations at startup.
- **Database:** the `payvy-db` accessory (Postgres 18.6), data in `/root/payvy-db/data`. The
  init script `deploy/postgres-init/01-app-role.sh` creates the `payvy` role, which owns the
  database but is not a superuser, so row-level security holds in production.
- **Database backups:** nightly at 09:17 UTC, root's crontab on the host runs
  `deploy/backup-db.sh` (installed to `/root/bin` by `deploy/install-backups.sh`). It dumps as the
  Postgres superuser inside `payvy-db` (so row-level security hides nothing), encrypts with
  `BACKUP_ENCRYPTION_KEY` (AES-256, PBKDF2) and uploads to the B2 bucket `payvy-db-backups`
  under `backups/{daily,weekly,monthly}/`. The bucket's lifecycle rules keep dailies 30 days,
  weeklies 90 and monthlies a year; the script never deletes. Credentials live in
  `/root/payvy-backup.env` (mode 600), written from 1Password (vault Payvy: "Payvy prod - B2
  payvy-db-backups keyID/applicationKey", "Payvy prod - DB backup encryption key"). Log:
  `/var/log/payvy-backup.log`. `deploy/restore-check.sh` restores the newest backup into a
  scratch database and compares row counts with the live one; run it now and then. Restoring
  for real: download and decrypt as restore-check does, then `pg_restore` into a fresh database.
  Without the encryption key the backups are unreadable, and without `PAYVY_SECRETS_KEY` a
  restore still needs every bank reconnected; both are in 1Password.
- **API traffic:** Cloudflare DNS (`dev.payvy.com`, DNS-only A record) → host nginx, TLS from
  certbot (`deploy/nginx-payvy-beta.conf`) → kamal-proxy on `127.0.0.1:8090` → the app on
  7430. kamal-proxy is shared: its `proxy.run` settings must match the other apps on the box.
  Host nginx blocks `/operator` (`location ^~ /operator { return 404; }`), and Kamal does **not**
  deploy `/etc/nginx/sites-available/payvy` — always apply and reload `deploy/nginx-payvy-beta.conf`
  via `deploy/setup-host.sh` on `thatch-api-prod` **before** setting `PAYVY_OPERATOR_KEY` in Kamal.
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
   password, `PAYVY_API_KEY`, `PAYVY_INBOUND_SECRET`, `PAYVY_SECRETS_KEY` (seals bank tokens;
   losing it means reconnecting every bank). Add an R2 API token scoped to
   `payvy-documents` (Object Read & Write) as two items, and an Anthropic API key for reading
   documents (`PAYVY_EXTRACT_API_KEY`), and an IAM access key allowed only `ses:SendEmail` in
   us-west-2, where payvy.com is verified (`PAYVY_SES_ACCESS_KEY_ID`, `PAYVY_SES_SECRET_ACCESS_KEY`).
   Put the references in `.kamal/secrets`.
2. Cloudflare: A record `dev.payvy.com` → 5.78.94.150 (DNS only); deploy `payvy-web`
   for the `beta.payvy.com` custom domain. Email
   Routing → add the subdomain `in.payvy.com` (done 2026-11-10). Hosted Payvy sends through
   SES, not Cloudflare Email Service, so no sending setup is needed in Cloudflare.
3. On the host: copy the nginx file to `/etc/nginx/sites-available/payvy`, link it, get the
   certificate (`certbot certonly --webroot -w /var/www/payvy-acme -d dev.payvy.com -d beta.payvy.com`), then
   `nginx -t && systemctl reload nginx`.
4. `kamal accessory boot db`, then `kamal deploy`.
5. Legacy worker: `wrangler secret put PAYVY_INBOUND_SECRET` and deploy it.
6. Let the first person sign in (`kamal app exec --reuse "payvy person add --email ... --name ..."`; `--reuse` runs it in the live container),
   open https://beta.payvy.com and ask for a link. Create the inbound address
   (`email.new_address`), then send a test bill.
7. Paying through Mercury: in direct mode, add 5.78.94.150 to the token's IP allowlist; request
   mode needs none. Connect with `bank.connect` (see `docs/payments.md`).
