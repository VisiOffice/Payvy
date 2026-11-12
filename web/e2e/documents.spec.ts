// Adding an invoice from the browser: it's uploaded, read, and opened for review.
import { expect, test } from '@playwright/test';
import { vendor } from './api';
import { pdf } from './pdf';

test('an uploaded invoice is read and opened for review', async ({ page }) => {
  await vendor('Sonoran Pest Control');
  await page.goto('/inbox');
  await page.locator('input[type=file]').setInputFiles({
    name: 'sonoran-sr-7781.pdf',
    mimeType: 'application/pdf',
    buffer: pdf(['Sonoran Pest Control', 'Invoice SR-7781', 'Date 11/03/2026', 'Total 185.00']),
  });

  await expect(page).toHaveURL(/\/inbox\/[0-9a-f-]{36}$/);
  await expect(page.getByLabel('Invoice number')).toHaveValue('SR-7781');
  await expect(page.getByLabel('Amount')).toHaveValue('185.00');
  await expect(page.getByText(/Matched Sonoran Pest Control/)).toBeVisible();
  await expect(page.locator('.payvy-hl[data-hl="invoice_number"]')).toBeVisible();

  // The same file again is refused, and says so.
  await page.goto('/inbox');
  await page.locator('input[type=file]').setInputFiles({
    name: 'again.pdf',
    mimeType: 'application/pdf',
    buffer: pdf(['Sonoran Pest Control', 'Invoice SR-7781', 'Date 11/03/2026', 'Total 185.00']),
  });
  await expect(page.getByRole('region', { name: 'Notifications' }).getByText(/already/)).toBeVisible();
  await expect(page.getByRole('link', { name: 'sonoran-sr-7781.pdf' })).toBeVisible();
});
