import { expect, test } from '@playwright/test';
import { agent, vendor } from './api';

test('a person adds a vendor before its first bill and lands on its page', async ({ page }) => {
  const name = `Added ahead ${Date.now()}`;
  await page.goto('/vendors');
  await page.getByRole('button', { name: 'Add vendor' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Name').fill(name);
  await dialog.getByLabel('Email').fill('ar@ahead.example');
  await dialog.getByLabel('Payment terms (days)').fill('30');
  await dialog.getByRole('button', { name: 'Add vendor' }).click();
  await expect(page).toHaveURL(/\/vendors\/[0-9a-f-]{36}$/);
  await expect(page.getByRole('heading', { level: 1, name })).toBeVisible();
  const { vendors } = await agent<{ vendors: { name: string }[] }>('vendor.list', {});
  expect(vendors.filter((v) => v.name === name)).toHaveLength(1);
});

test('adding a vendor that already exists points to the one on file', async ({ page }) => {
  const name = `Already here ${Date.now()}`;
  await vendor(name);
  await page.goto('/vendors');
  await page.getByRole('button', { name: 'Add vendor' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Name').fill(name.toUpperCase());
  await expect(dialog.getByText(`${name} is already a vendor`)).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'Add vendor' })).toBeDisabled();
});
