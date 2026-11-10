// Bank settings against the fake Mercury and Rho (fake-bank.ts), reached
// through sandbox connections. Tests share a database and the bill tests
// expect no paying bank, so this one leaves Mercury disconnected.
import { expect, test } from '@playwright/test';

test('a person connects Rho to read and Mercury to pay, then disconnects', async ({ page }) => {
  await page.goto('/settings/bank');
  await expect(page.getByRole('heading', { name: 'Bank', level: 1 })).toBeVisible();
  await expect(page.getByText('No bank pays bills yet.', { exact: false })).toBeVisible();

  // Rho: a refused token says so; a good one shows the balances.
  const rhoToken = page.getByLabel('Rho API token');
  await rhoToken.fill('bad');
  await page.getByRole('checkbox', { name: 'Sandbox account' }).last().check();
  await page.getByRole('button', { name: 'Connect Rho' }).click();
  await expect(page.getByText(/Rho refused this token/)).toBeVisible();
  await rhoToken.fill('rho-good');
  await page.getByRole('button', { name: 'Connect Rho' }).click();
  await expect(page.getByText('Cash (Checking)')).toBeVisible();
  await expect(page.getByText('$117,263.53')).toBeVisible();

  // Mercury: the token reaches two accounts, so Payvy asks which pays bills.
  await page.getByLabel('Mercury API token').fill('merc-good');
  await page.getByRole('checkbox', { name: 'Sandbox account' }).first().check();
  await page.getByRole('button', { name: 'Connect Mercury' }).click();
  await page.getByRole('combobox', { name: 'Pay bills from' }).click();
  await page.getByRole('option', { name: 'Mercury Payroll ••7781' }).click();
  await page.getByRole('button', { name: 'Connect Mercury' }).click();
  await expect(page.getByRole('definition').filter({ hasText: 'Mercury Payroll ••7781' })).toBeVisible();
  await expect(page.getByText('Each payment waits in Mercury for an admin to approve it')).toBeVisible();

  // Both stay: Rho never replaced the paying bank.
  await expect(page.getByText('Cash (Checking)')).toBeVisible();

  // Disconnect both, each behind a confirmation.
  for (const consequence of [/No new payments go through Mercury/, /Payvy stops reading Rho/]) {
    await page.getByRole('button', { name: 'Disconnect' }).first().click();
    const dialog = page.getByRole('alertdialog');
    await expect(dialog.getByText(consequence)).toBeVisible();
    await dialog.getByRole('button', { name: 'Disconnect' }).click();
    await expect(dialog).toHaveCount(0);
  }
  await expect(page.getByText('No bank pays bills yet.', { exact: false })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Connect Rho' })).toBeVisible();
});
