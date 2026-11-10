import { expect, test } from '@playwright/test';
import { agent } from './api';

test('a person creates the bill inbox address, then replaces it', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('link', { name: 'Bill inbox' }).click();
  await expect(page).toHaveURL(/\/settings\/email$/);

  const address = page.getByLabel('Bill inbox address', { exact: true });
  const existing = await agent<{ address: string | null }>('email.address', {});
  if (!existing.address) {
    await expect(page.getByText('No bill inbox yet.')).toBeVisible();
    await page.getByRole('button', { name: 'Create address' }).click();
  }
  await expect(address).toHaveText(/^[a-z0-9]{12}@in\.payvy\.test$/);
  const first = (await address.textContent())!;
  expect((await agent<{ address: string }>('email.address', {})).address).toBe(first);

  await page.getByRole('button', { name: 'Replace address…' }).click();
  await expect(page.getByRole('alertdialog')).toContainText(`${first} stops working at once`);
  await page.getByRole('button', { name: 'Replace', exact: true }).click();
  await expect(address).not.toHaveText(first);
  const second = (await address.textContent())!;
  expect((await agent<{ address: string }>('email.address', {})).address).toBe(second);
});

test('agents can read the address but not create one', async () => {
  await expect(agent('email.new_address', {})).rejects.toThrow();
});
