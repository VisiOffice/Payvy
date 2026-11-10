import { expect, test } from '@playwright/test';
import { EDGE_PORT } from './env';

test.skip(!EDGE_PORT, 'Set PAYVY_E2E_EDGE_PORT to exercise the Cloudflare Worker');

test('edge serves deep links and static assets without treating API errors as HTML', async ({ request }) => {
  const deep = await request.get('/vendors/example');
  expect(deep.status()).toBe(200);
  expect(deep.headers()['content-type']).toContain('text/html');
  expect(deep.headers()['x-robots-tag']).toBe('noindex, nofollow');
  const font = await request.get('/fonts/InterVariable.woff2');
  expect(font.ok()).toBe(true);
  expect((await font.body()).length).toBeGreaterThan(100_000);
  const missing = await request.post('/api/commands/does.not.exist', {
    headers: { 'x-payvy-client': 'web' }, data: { input: {}, mode: 'commit' },
  });
  expect(missing.ok()).toBe(false);
  expect(missing.headers()['content-type']).toContain('application/json');
  expect(missing.headers()['cache-control']).toBe('no-store');
});

test('edge preserves session cookies and the command CSRF requirement', async ({ request }) => {
  const me = await request.get('/auth/session');
  expect(me.ok()).toBe(true);
  expect((await me.json()).person.name).toBe('Robert');
  expect(me.headers()['cache-control']).toBe('no-store');
  const refused = await request.post('/api/commands/person.list', { data: { input: {}, mode: 'commit' } });
  expect(refused.status()).toBe(401);
  const allowed = await request.post('/api/commands/person.list', {
    headers: { 'x-payvy-client': 'web' }, data: { input: {}, mode: 'commit' },
  });
  expect(allowed.ok()).toBe(true);
});

test('cross-origin sign-out is rejected without losing the session', async ({ request }) => {
  const rejected = await request.delete('/auth/session', { headers: { Origin: 'https://unrelated.example' } });
  expect(rejected.status()).toBe(403);
  expect((await request.get('/auth/session')).ok()).toBe(true);
});
