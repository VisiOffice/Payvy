// Adding an invoice from the browser: it's uploaded, read, and opened for review.
import { expect, test, type Page } from '@playwright/test';
import { vendor } from './api';
import { pdf } from './pdf';

/** A file drag on the inbox. `dragenter` shows the overlay; `drop` hands the file over. */
async function dragFile(page: Page, kind: 'dragenter' | 'drop', file: { name: string; type: string; base64?: string; bytes?: number }) {
  await page.evaluate(({ kind, name, type, base64, bytes }) => {
    const data = base64
      ? Uint8Array.from(atob(base64), (c) => c.charCodeAt(0))
      : new Uint8Array(bytes!);
    const dt = new DataTransfer();
    dt.items.add(new File([data], name, { type }));
    window.dispatchEvent(new DragEvent(kind, { bubbles: true, cancelable: true, dataTransfer: dt }));
  }, { kind, name: file.name, type: file.type, base64: file.base64, bytes: file.bytes });
}

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

test('dropping a PDF on the inbox adds it, and a file over 25 MB is refused by name', async ({ page }) => {
  const number = `DR-${String(Date.now()).slice(-6)}`;
  await vendor('Dropped Hauling');
  const file = pdf(['Dropped Hauling', `Invoice ${number}`, 'Date 11/03/2026', 'Total 42.00']);
  await page.goto('/inbox');

  await dragFile(page, 'dragenter', { name: 'dropped.pdf', type: 'application/pdf', base64: file.toString('base64') });
  await expect(page.getByText('Drop to add invoices')).toBeVisible();
  await dragFile(page, 'drop', { name: 'dropped.pdf', type: 'application/pdf', base64: file.toString('base64') });
  await expect(page).toHaveURL(/\/inbox\/[0-9a-f-]{36}$/);
  await expect(page.getByLabel('Invoice number')).toHaveValue(number);

  await page.goto('/inbox');
  await dragFile(page, 'dragenter', { name: 'too-big.pdf', type: 'application/pdf', bytes: 26 * 1024 * 1024 });
  await expect(page.getByText('Drop to add invoices')).toBeVisible();
  await dragFile(page, 'drop', { name: 'too-big.pdf', type: 'application/pdf', bytes: 26 * 1024 * 1024 });
  await expect(page.getByRole('region', { name: 'Notifications' }).getByText('too-big.pdf is larger than 25 MB')).toBeVisible();
  await expect(page).toHaveURL(/\/inbox$/);
});
