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

test('a person edits a vendor and manages its account numbers', async ({ page }) => {
  const name = `Misspeled ${Date.now()}`;
  const id = await vendor(name);
  await page.goto(`/vendors/${id}`);

  await page.getByRole('button', { name: 'Edit vendor' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByLabel('Name')).toHaveValue(name);
  const fixed = name.replace('Misspeled', 'Misspelled');
  await dialog.getByLabel('Name').fill(fixed);
  await dialog.getByLabel('Email').fill('ar@fixed.example');
  await expect(dialog.getByText(/new payment-detail invitations go to it/)).toBeVisible();
  await dialog.getByLabel('Payment terms (days)').fill('45');
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole('heading', { level: 1, name: fixed })).toBeVisible();
  await expect(page.getByText(/ar@fixed\.example · Net 45/)).toBeVisible();

  await page.getByLabel('Account number').fill('99-4417');
  await page.getByLabel('Location').fill('1420 Elm St site');
  await page.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(page.getByText('1420 Elm St site')).toBeVisible();
  await page.getByRole('button', { name: 'Remove account number 99-4417' }).click();
  await expect(page.getByText('1420 Elm St site')).toHaveCount(0);

  const detail = await agent<{ vendor: { name: string; email: string; terms_days: number }; accounts: unknown[] }>(
    'vendor.get',
    { vendor_id: id },
  );
  expect(detail.vendor).toMatchObject({ name: fixed, email: 'ar@fixed.example', terms_days: 45 });
  expect(detail.accounts).toHaveLength(0);
});
