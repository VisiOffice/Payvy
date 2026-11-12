import { expect, test, type Page } from '@playwright/test';
import { agent, bill, vendor } from './api';

/** Dark mode, as a person picks it in Settings > Preferences, then back to the page. */
async function dark(page: Page) {
  const here = new URL(page.url()).pathname;
  await page.goto('/settings/preferences');
  await page.getByRole('radiogroup', { name: 'Appearance' }).getByRole('radio', { name: 'Dark', exact: true }).click();
  await page.goto(here);
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
}

test('company menu opens settings, which has its own navigation', async ({ page }, testInfo) => {
  await page.goto('/bills');
  const trigger = page.getByRole('button', { name: 'Default, company menu', exact: true });
  await trigger.focus();
  await page.keyboard.press('Enter');
  const settings = page.getByRole('menuitem', { name: 'Settings' });
  await expect(settings).toBeVisible();
  await expect(settings).toContainText('then');
  await page.screenshot({ path: testInfo.outputPath('company-menu-desktop.png'), animations: 'disabled' });
  await settings.click();
  await expect(page).toHaveURL('/settings/preferences');
  // The app's own pages leave the nav; settings are grouped in their place.
  await expect(page.getByRole('link', { name: 'All bills' })).toHaveCount(0);
  for (const group of ['Personal', 'Bills in', 'Paying']) await expect(page.getByText(group, { exact: true })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Company', exact: true })).toBeVisible();
  await page.getByRole('link', { name: 'Bank', exact: true }).click();
  await expect(page).toHaveURL('/settings/bank');
  await page.getByRole('link', { name: 'Workflows', exact: true }).click();
  await expect(page).toHaveURL('/settings/workflows');
  await page.screenshot({ path: testInfo.outputPath('settings-desktop.png'), animations: 'disabled' });
  // Back to app returns to where you were, not the start.
  await page.getByRole('link', { name: 'Back to app' }).click();
  await expect(page).toHaveURL('/bills');
  await expect(page.getByRole('link', { name: 'Bank', exact: true })).toHaveCount(0);

  await page.setViewportSize({ width: 390, height: 844 });
  await trigger.click();
  await expect(page.getByRole('menuitem', { name: 'Invite and manage people' })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath('company-menu-mobile.png'), animations: 'disabled' });
  await page.getByRole('menuitem', { name: 'Invite and manage people' }).click();
  await expect(page).toHaveURL('/settings/people');
  await page.getByRole('button', { name: 'Open navigation' }).click();
  const drawer = page.getByRole('dialog', { name: 'Navigation' });
  await expect(drawer.getByRole('link', { name: 'Back to app' })).toBeVisible();
  await expect(drawer.getByRole('link', { name: 'Bank', exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath('settings-mobile.png'), animations: 'disabled' });
});

test('G then S opens settings, but not while typing', async ({ page }) => {
  await page.goto('/bills');
  const search = page.getByRole('searchbox', { name: 'Search bills' });
  await search.click();
  await page.keyboard.type('gs');
  await expect(search).toHaveValue('gs');
  await expect(page).toHaveURL('/bills');
  await search.blur();
  await page.keyboard.press('g');
  await page.keyboard.press('s');
  await expect(page).toHaveURL('/settings/preferences');
  // The old address of the default workflow still works.
  await page.goto('/workflows');
  await expect(page).toHaveURL('/settings/workflows');
});

test('appearance persists, overrides the OS, and can follow it again', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' });
  await page.goto('/settings/preferences');
  const appearance = page.getByRole('radiogroup', { name: 'Appearance' });
  await expect(appearance.getByRole('radio', { name: 'System' })).toBeChecked();
  const textColor = () => page.locator('.payvy-page').evaluate(el => getComputedStyle(el).color);
  const light = await textColor();
  await appearance.getByRole('radio', { name: 'Dark', exact: true }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.reload();
  await expect(appearance.getByRole('radio', { name: 'Dark', exact: true })).toBeChecked();
  await expect.poll(textColor).not.toBe(light);
  await page.emulateMedia({ colorScheme: 'dark' });
  await appearance.getByRole('radio', { name: 'Light', exact: true }).click();
  await expect.poll(textColor).toBe(light);
  await appearance.getByRole('radio', { name: 'System' }).click();
  await expect.poll(textColor).not.toBe(light);
  await page.emulateMedia({ colorScheme: 'light' });
  await expect.poll(textColor).toBe(light);
});

test('bill search, status and sorting work together', async ({ page }, testInfo) => {
  const vendorId = await vendor('Juniper Design Test');
  await agent('bill.create', { vendor_id: vendorId, invoice_number: 'DESIGN-LOW', invoice_date: '2026-11-01', due_date: '2026-12-01', amount_cents: 12500 });
  await agent('bill.create', { vendor_id: vendorId, invoice_number: 'DESIGN-HIGH', invoice_date: '2026-11-05', due_date: '2026-12-05', amount_cents: 75000 });
  await page.goto('/bills');
  await page.getByRole('searchbox', { name: 'Search bills' }).fill('juniper design');
  await expect(page.locator('tbody tr')).toHaveCount(2);
  await page.getByRole('combobox', { name: 'Sort bills' }).click();
  await page.getByRole('option', { name: 'Largest amount' }).click();
  await expect(page.locator('tbody tr').first()).toContainText('DESIGN-HIGH');
  await page.getByRole('combobox', { name: 'Sort bills' }).click();
  await page.getByRole('option', { name: 'Due date', exact: true }).click();
  await expect(page.locator('tbody tr').first()).toContainText('DESIGN-LOW');
  await page.getByRole('combobox', { name: 'Bill status' }).click();
  await page.getByRole('option', { name: 'Paid', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'No matching bills' })).toBeVisible();
  await page.getByRole('combobox', { name: 'Bill status' }).click();
  await page.getByRole('option', { name: 'All statuses' }).click();
  await page.getByRole('searchbox', { name: 'Search bills' }).fill('design-low');
  await expect(page.locator('tbody tr')).toHaveCount(1);
  await expect(page.locator('tbody tr')).toContainText('DESIGN-LOW');
  await page.getByRole('searchbox', { name: 'Search bills' }).fill('');
  await page.screenshot({ path: testInfo.outputPath('bills-light.png'), animations: 'disabled' });
  await dark(page);
  await page.screenshot({ path: testInfo.outputPath('bills-dark.png'), animations: 'disabled' });
  await page.goto(`/vendors/${vendorId}`);
  await expect(page.getByRole('button', { name: 'Edit workflow', exact: true })).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('vendor-dark.png'), animations: 'disabled' });
});

test('mobile queue actions stay reachable and update the navigation count', async ({ page }, testInfo) => {
  const vendorId = await vendor('Ponderosa Maintenance');
  await bill({ vendor_id: vendorId, invoice_number: 'MOBILE-1', invoice_date: '2026-11-01', amount_cents: 142000 });
  await page.goto('/');
  const count = page.locator('.payvy-nav-count');
  await expect(count).toBeVisible();
  const before = Number(await count.textContent());
  await page.screenshot({ path: testInfo.outputPath('inbox-light.png'), animations: 'disabled' });
  await dark(page);
  await page.screenshot({ path: testInfo.outputPath('inbox-dark.png'), animations: 'disabled' });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole('heading', { name: 'Waiting on you' })).toBeVisible();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.getByRole('radio', { name: 'Payments', exact: true }).click();
  await expect(page.getByRole('region', { name: 'Bills to pay' })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath('inbox-mobile.png'), animations: 'disabled' });
  const row = page.locator('.payvy-queue-row').filter({ hasText: 'Ponderosa Maintenance' });
  await row.getByRole('button', { name: 'Reject', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Why').fill('Incorrect invoice');
  await dialog.getByRole('button', { name: 'Reject', exact: true }).click();
  await expect(row).toHaveCount(0);
  await page.setViewportSize({ width: 1440, height: 900 });
  await expect(count).toHaveText(String(before - 1));
});
