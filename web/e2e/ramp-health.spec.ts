import { expect, test, type Page } from '@playwright/test';
import { state } from './api';

async function fakeConfig(input: object) {
  const url = state().mercuryUrl.replace(/\/mercury$/, '/ramp/test/config');
  return (await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(input) })).json();
}
async function authorize(page: Page) {
  await page.getByRole('button', { name: 'Authorize Ramp', exact: true }).click();
  await expect(page.getByText('Ramp Test Company · sandbox')).toBeVisible();
  await page.getByRole('combobox', { name: 'Ramp entity and funding account' }).click();
  await page.getByRole('option', { name: 'US Operations · Operating …0042' }).click();
  await page.getByRole('button', { name: 'Review Ramp activation' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Select Ramp', exact: true }).click();
  await expect(page.getByText('Access renews automatically', { exact: true })).toBeVisible();
}

test('automatic renewal detects revoked access and a person reconnects in Bank settings', async ({ page }) => {
  // Use a short fake grant and the real worker/clock; no application responses
  // or database rows are mocked. The worker checks every thirty seconds.
  test.setTimeout(180_000);
  await fakeConfig({ reset: true, tokenTtl: 95 });
  await page.route('https://demo.ramp.com/v1/authorize?**', async route => {
    const url = new URL(route.request().url());
    const callback = new URL(url.searchParams.get('redirect_uri')!);
    callback.searchParams.set('state', url.searchParams.get('state')!);
    callback.searchParams.set('code', 'fake-code');
    await route.fulfill({ status: 302, headers: { location: callback.toString() } });
  });
  try {
    await page.goto('/settings/bank');
    await authorize(page);
    await fakeConfig({ tokenStatus: 401 });
    // The UI polls health, not the provider, while the worker renews the grant.
    await expect(page.getByText('Needs reconnection', { exact: true })).toBeVisible({ timeout: 130_000 });
    await expect(page.getByText('Ramp rejected the authorization. Reconnect Ramp to restore access.')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Check Ramp connection' })).toHaveCount(0);
    expect(await page.locator('body').innerText()).not.toContain('fake-secret-error');
    const counts = await fakeConfig({});
    expect(counts.tokenRequests).toBe(2);
    expect(counts.writes).toBe(0);
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: 'test-results/ramp-reconnect-mobile.png', fullPage: true });
    await page.getByRole('link', { name: 'Reconnect Ramp', exact: true }).click();
    await fakeConfig({ tokenStatus: 200, tokenTtl: 3600 });
    await authorize(page);
    await expect(page.getByText('Needs reconnection', { exact: true })).toHaveCount(0);
    await expect(page.getByText('Connected', { exact: true })).toBeVisible();
  } finally {
    await fakeConfig({ reset: true });
    await page.request.post('/api/commands/bank.disconnect', { headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input: { provider: 'ramp' } } });
  }
});
