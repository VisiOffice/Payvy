// Where the end-to-end run lives. Ports differ from development (7430) so a
// dev server can keep running.
import path from 'node:path';

export const ROOT = path.resolve(import.meta.dirname, '../..');
export const STATE_DIR = path.join(import.meta.dirname, '.state');
/** The signed-in browser state global setup saves. */
export const SIGNED_IN = path.join(STATE_DIR, 'signed-in.json');
/** What global setup made, for the tests: see State. */
export const STATE_FILE = path.join(STATE_DIR, 'state.json');

export const PORT = Number(process.env.PAYVY_E2E_PORT ?? 7431);
export const MODEL_PORT = Number(process.env.PAYVY_E2E_MODEL_PORT ?? 7432);
export const BANK_PORT = Number(process.env.PAYVY_E2E_BANK_PORT ?? 7433);
export const EDGE_PORT = process.env.PAYVY_E2E_EDGE_PORT ? Number(process.env.PAYVY_E2E_EDGE_PORT) : null;
export const API_URL = `http://127.0.0.1:${PORT}`;
export const BASE_URL = `http://127.0.0.1:${EDGE_PORT ?? PORT}`;

// The docker compose stores (see .env.example).
export const ADMIN_URL = process.env.PAYVY_TEST_ADMIN_URL ?? 'postgres://postgres:postgres@127.0.0.1:5446/postgres';
export const APP_URL = process.env.PAYVY_TEST_APP_URL ?? 'postgres://payvy:payvy@127.0.0.1:5446';
export const S3 = {
  endpoint: process.env.PAYVY_TEST_S3_ENDPOINT ?? 'http://127.0.0.1:8336',
  accessKey: process.env.PAYVY_TEST_S3_ACCESS_KEY ?? 'payvy-dev',
  secretKey: process.env.PAYVY_TEST_S3_SECRET_KEY ?? 'payvy-dev-secret',
  bucket: 'payvy-e2e',
};

export const PERSON = { email: 'robert@example.com', name: 'Robert', actor: 'robert' };

export interface State {
  /** An agent key (claude-ap) for setting up data the way an agent would. */
  agentKey: string;
  mercuryUrl: string;
  serverLog: string;
}
