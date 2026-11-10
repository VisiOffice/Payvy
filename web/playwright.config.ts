import { defineConfig, devices } from '@playwright/test';
import { BASE_URL, SIGNED_IN } from './e2e/env';

// End-to-end tests: the real server (target/debug/payvy) serving the built
// app (web/dist), against a fresh database and the docker compose stores.
// `pnpm e2e` builds both first. One worker: the tests share one database
// and one server log, and read sign-in links from it in order.
export default defineConfig({
  testDir: './e2e',
  globalSetup: './e2e/global-setup.ts',
  workers: 1,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: BASE_URL,
    storageState: SIGNED_IN,
    trace: 'retain-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], viewport: { width: 1440, height: 900 } } }],
});
