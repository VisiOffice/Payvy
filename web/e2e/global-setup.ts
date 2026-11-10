// Starts a real Payvy for the run: a fresh database, the docker compose S3
// store, a fake model for extraction, and the built web app. Signs one
// person in through an emailed link. Returns the teardown.
import { execFileSync, spawn } from 'node:child_process';
import { randomBytes, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import pg from 'pg';
import { startFakeBank } from './fake-bank';
import { startFakeModel } from './fake-model';
import { linkAfter, logMark } from './mail';
import {
  ADMIN_URL,
  APP_URL,
  API_URL,
  BASE_URL,
  EDGE_PORT,
  MODEL_PORT,
  BANK_PORT,
  PERSON,
  PORT,
  ROOT,
  S3,
  SIGNED_IN,
  STATE_DIR,
  STATE_FILE,
  type State,
} from './env';

const BIN = path.join(ROOT, 'target/debug/payvy');
const WEB = path.join(ROOT, 'web/dist');

async function waitForHealth(server: ReturnType<typeof spawn>, log: string) {
  const until = Date.now() + 30_000;
  for (;;) {
    if (server.exitCode !== null) {
      throw new Error(`payvy serve exited (${server.exitCode}):\n${fs.readFileSync(log, 'utf8')}`);
    }
    try {
      if ((await fetch(`${API_URL}/health`)).ok) return;
    } catch {
      // not listening yet
    }
    if (Date.now() > until) throw new Error(`payvy serve didn't come up:\n${fs.readFileSync(log, 'utf8')}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

export default async function globalSetup() {
  for (const [what, p, hint] of [
    ['the server', BIN, 'cargo build -p payvy-server'],
    ['the web app', path.join(WEB, 'index.html'), 'pnpm --dir web build'],
  ] as const) {
    if (!fs.existsSync(p)) throw new Error(`${what} isn't built (${p}); run ${hint}, or use pnpm e2e`);
  }
  fs.rmSync(STATE_DIR, { recursive: true, force: true });
  fs.mkdirSync(STATE_DIR, { recursive: true });

  // A database of its own, dropped at the end. The name is generated, so
  // formatting it into SQL is safe.
  const db = `payvy_e2e_${randomUUID().replaceAll('-', '')}`;
  const admin = new pg.Client({ connectionString: ADMIN_URL });
  await admin.connect();
  await admin.query(`CREATE DATABASE ${db} OWNER payvy`);
  await admin.end();

  const model = await startFakeModel(MODEL_PORT);
  const bank = await startFakeBank(BANK_PORT);

  // Only what this run needs: nothing from the developer's shell that could
  // point it at a real bank, mailer or model.
  const env: NodeJS.ProcessEnv = {
    PATH: process.env.PATH,
    HOME: process.env.HOME,
    RUST_LOG: 'info',
    NO_COLOR: '1',
    DATABASE_URL: `${APP_URL}/${db}`,
    PAYVY_BIND: `127.0.0.1:${PORT}`,
    PAYVY_API_KEY: randomBytes(24).toString('hex'),
    PAYVY_PUBLIC_URL: BASE_URL,
    PAYVY_WEB_DIR: WEB,
    PAYVY_S3_ENDPOINT: S3.endpoint,
    PAYVY_S3_BUCKET: S3.bucket,
    PAYVY_S3_REGION: 'us-east-1',
    PAYVY_S3_ACCESS_KEY: S3.accessKey,
    PAYVY_S3_SECRET_KEY: S3.secretKey,
    PAYVY_S3_CREATE_BUCKET: 'true',
    PAYVY_EXTRACT_AUTO: 'false',
    PAYVY_EXTRACT_PROVIDER: 'openai-compatible',
    PAYVY_EXTRACT_MODEL: 'fake',
    PAYVY_EXTRACT_BASE_URL: `http://127.0.0.1:${MODEL_PORT}`,
    PAYVY_EXTRACT_API_KEY: 'fake',
    PAYVY_SECRETS_KEY: randomBytes(32).toString('hex'),
    PAYVY_INBOUND_DOMAIN: 'in.payvy.test',
    PAYVY_INBOUND_SECRET: randomBytes(32).toString('hex'),
    PAYVY_MERCURY_SANDBOX_URL: `http://127.0.0.1:${BANK_PORT}/mercury`,
    PAYVY_RHO_SANDBOX_URL: `http://127.0.0.1:${BANK_PORT}/rho`,
  };
  // The bank-only run needs no document store. Full suites still use real S3.
  if (process.env.PAYVY_E2E_BANK_ONLY === 'true') {
    for (const key of Object.keys(env)) if (key.startsWith('PAYVY_S3_')) delete env[key];
  }
  const serverLog = path.join(STATE_DIR, 'server.log');
  const out = fs.openSync(serverLog, 'w');
  const server = spawn(BIN, ['serve'], { env, stdio: ['ignore', out, out] });
  await waitForHealth(server, serverLog);

  // Exercise the same asset routing and streaming proxy as Cloudflare, against
  // this run's isolated Rust server. No production API or bank is contacted.
  const edge = EDGE_PORT ? await (await import('wrangler')).unstable_dev(
    path.join(ROOT, 'web/worker/index.ts'), {
      config: path.join(ROOT, 'web/wrangler.jsonc'), env: 'local',
      ip: '127.0.0.1', port: EDGE_PORT, inspectorPort: 0, local: true,
      vars: { API_ORIGIN: API_URL }, persist: false, logLevel: 'error',
      experimental: { disableExperimentalWarning: true, disableDevRegistry: true, watch: false },
    },
  ) : null;

  const cli = (...args: string[]) => execFileSync(BIN, args, { env, encoding: 'utf8', stdio: 'pipe' });
  cli('person', 'add', '--email', PERSON.email, '--name', PERSON.name);
  const agentKey = /pv_[0-9a-f]+/.exec(cli('key', 'create', '--kind', 'agent', '--actor', 'claude-ap'))?.[0];
  if (!agentKey) throw new Error('payvy key create printed no key');

  // Sign in the way a person does, and keep the cookie for the tests.
  const mark = logMark(serverLog);
  await fetch(`${BASE_URL}/auth/link`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ email: PERSON.email }),
  });
  const token = (await linkAfter(serverLog, mark)).split('#token=')[1];
  const res = await fetch(`${BASE_URL}/auth/session`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ token }),
  });
  const cookie = /payvy_session=([0-9a-f]+)/.exec(res.headers.get('set-cookie') ?? '')?.[1];
  if (!res.ok || !cookie) throw new Error(`signing in failed: ${res.status} ${await res.text()}`);
  fs.writeFileSync(
    SIGNED_IN,
    JSON.stringify({
      cookies: [
        {
          name: 'payvy_session',
          value: cookie,
          domain: '127.0.0.1',
          path: '/',
          expires: -1,
          httpOnly: true,
          secure: false,
          sameSite: 'Lax',
        },
      ],
      origins: [],
    }),
  );
  fs.writeFileSync(STATE_FILE, JSON.stringify({ agentKey, serverLog, mercuryUrl: `http://127.0.0.1:${BANK_PORT}/mercury` } satisfies State));

  return async () => {
    await edge?.stop();
    server.kill('SIGTERM');
    await new Promise((r) => server.once('exit', r));
    model.close();
    bank.close();
    const admin = new pg.Client({ connectionString: ADMIN_URL });
    await admin.connect();
    await admin.query(`DROP DATABASE IF EXISTS ${db} WITH (FORCE)`);
    await admin.end();
  };
}
