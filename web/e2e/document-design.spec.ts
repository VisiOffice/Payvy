import { expect, test } from '@playwright/test';
import { agent, vendor } from './api';
import { pdf } from './pdf';

async function sample(tag: string) {
  const bytes = pdf(['King Environmental, LLC', 'Invoice 189727', 'Date 11/01/2026', 'Due 12/01/2026', '30 yd roll-off haul 300.00', 'Disposal 125.00', 'Tax 34.00', 'Total $459.00', tag]);
  const doc = await agent<{ id: string }>('document.upload', { filename: `king-environmental-${tag}.pdf`, content_base64: bytes.toString('base64') });
  await agent('document.extract', { document_id: doc.id });
  return doc.id;
}

test('document workspace keeps actions visible, fits the original, and works in both themes and on a phone', async ({ page }, info) => {
  const existing = await agent<{ vendors: { name: string }[] }>('vendor.list', {});
  if (!existing.vendors.some(v => v.name === 'King Environmental, LLC')) await vendor('King Environmental, LLC');
  const id = await sample('design');
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.goto(`/inbox/${id}`);
  await expect(page.getByLabel('Invoice number')).toHaveValue('189727');
  await expect(page.locator('canvas')).toBeVisible();
  const action = page.getByRole('button', { name: 'Create bill', exact: true });
  await expect(action).toBeInViewport();
  const paper = await page.locator('canvas').boundingBox();
  const preview = await page.locator('.payvy-viewer-canvas').boundingBox();
  expect(paper!.height).toBeLessThanOrEqual(preview!.height);
  await page.screenshot({ path: info.outputPath('review-dark.png'), animations: 'disabled' });
  await page.getByRole('textbox', { name: 'Amount Required', exact: true }).focus();
  await expect(page.locator('.payvy-hl-focus[data-hl="total"]')).toBeVisible();
  await page.emulateMedia({ colorScheme: 'light' });
  await page.screenshot({ path: info.outputPath('review-light.png'), animations: 'disabled' });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole('radiogroup', { name: 'Document view' })).toBeVisible();
  await page.getByLabel('Memo').fill('Review this invoice');
  await page.getByRole('radio', { name: 'Original document', exact: true }).click();
  await expect(page.locator('canvas')).toBeVisible();
  await expect(action).toBeHidden();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath('review-mobile-original.png'), animations: 'disabled' });
  await page.getByRole('radio', { name: 'Invoice details', exact: true }).click();
  await expect(page.getByLabel('Memo')).toHaveValue('Review this invoice');
  await action.scrollIntoViewIfNeeded();
  await expect(action).toBeInViewport();
  await page.screenshot({ path: info.outputPath('review-mobile-details.png'), animations: 'disabled' });
  await page.goto('/inbox');
  await page.getByLabel('Search documents').fill('king-environmental-design');
  await expect(page.getByRole('link', { name: 'king-environmental-design.pdf', exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath('documents-mobile.png'), animations: 'disabled' });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.screenshot({ path: info.outputPath('documents-dark.png'), animations: 'disabled' });
});

test('a slow document load keeps context, times out, and can be retried without applying a late response', async ({ page }, info) => {
  const id = await sample('loading');
  await page.clock.install();
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  // Delay transport only. The response still comes from the real command API.
  await page.route('**/api/commands/document.get', async route => { await held; await route.continue(); });
  await page.goto(`/inbox/${id}`);
  await expect(page.getByText('Getting the original and its invoice details.')).toBeVisible();
  await expect(page.getByRole('link', { name: '← Inbox', exact: true })).toBeVisible();
  await page.screenshot({ path: info.outputPath('review-loading.png'), animations: 'disabled' });
  await page.clock.runFor(15_100);
  await expect(page.getByText("Couldn't load this document", { exact: true })).toBeVisible();
  const lateResponse = page.waitForResponse(r => r.url().endsWith('/api/commands/document.get'));
  release();
  await lateResponse;
  await expect(page.getByText("Couldn't load this document", { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Try again', exact: true }).click();
  await expect(page.getByLabel('Invoice number')).toHaveValue('189727');
  await expect(page.getByText("Couldn't load this document", { exact: true })).toHaveCount(0);
});
