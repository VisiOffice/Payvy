// A file of several invoices as one card per invoice: the header explains
// how the pages became invoices, pages move between cards with the Move
// menu (keyboard) or by dragging, and "Create N bills" splits the file at
// once while the server reads each part and makes a bill of every part that
// reads cleanly, stacking a copy of a bill already here on that bill.
import { expect, test, type Page } from '@playwright/test';
import { agent } from './api';
import { pdfPages } from './pdf';

const VENDOR = 'Saguaro Storage Co';

async function vendorId(): Promise<string> {
  const { vendors } = await agent<{ vendors: { id: string; name: string }[] }>('vendor.list', {});
  return vendors.find((v) => v.name === VENDOR)?.id ?? (await agent<{ id: string }>('vendor.create', { name: VENDOR })).id;
}

const card = (page: Page, name: string) => page.getByRole('region', { name, exact: true });
const explain = (page: Page) => page.locator('.payvy-split-explain');

test('several invoices as cards: moved by keyboard and by drag, then created as bills at once', async ({ page }, info) => {
  const vendor = await vendorId();
  // SS-103 is already a bill: the copy in this file stacks on it.
  await agent('bill.create', { vendor_id: vendor, invoice_number: 'SS-103', invoice_date: '2026-11-04', amount_cents: 4550 });

  const tag = `split-view-${Date.now()}`;
  const doc = await agent<{ id: string }>('document.upload', {
    filename: `${tag}.pdf`,
    content_base64: pdfPages([
      ['Saguaro Storage Co', 'Invoice SS-101', 'Continued', tag],
      ['Saguaro Storage Co', 'Invoice SS-101 continued', tag],
      ['Saguaro Storage Co', 'Invoice SS-101 total $300.00', tag],
      ['Saguaro Storage Co', 'Invoice SS-102 total $120.00', tag],
      ['Saguaro Storage Co', 'Invoice SS-103 total $45.50', tag],
      ['Saguaro Storage Co', 'Remittance slip', tag],
    ]).toString('base64'),
  });
  await page.goto(`/inbox/${doc.id}`);
  await page.getByRole('button', { name: 'Read it' }).click();

  // One line beside, the cards in the viewer, and a header that adds up.
  await expect(page.getByText('This file has 3 invoices.')).toBeVisible();
  await expect(explain(page)).toHaveText('6 pages → 3 invoices · SS-101 runs 3 pages · 1 page isn’t part of any bill');
  await expect(card(page, 'Invoice SS-101').getByText('pages 1–3 · $300.00')).toBeVisible();
  await expect(card(page, 'Not part of any bill').getByText('page 6 stay in To review')).toBeVisible();

  // Keyboard: page 3 out of SS-101 and back again, with the Move menu.
  await page.getByRole('button', { name: 'Move page 3' }).focus();
  await page.keyboard.press('Enter');
  await page.getByRole('menuitem', { name: 'Not part of any bill' }).press('Enter');
  await expect(explain(page)).toHaveText('6 pages → 3 invoices · Invoice 1 runs 2 pages · 2 pages aren’t part of any bill');
  // Changed pages aren't what was read: read again when created.
  await expect(card(page, 'Invoice 1').getByText('pages 1–2 · read when created')).toBeVisible();
  await page.getByRole('button', { name: 'Move page 3' }).focus();
  await page.keyboard.press('Enter');
  await page.getByRole('menuitem', { name: 'To Invoice 1' }).press('Enter');
  await expect(card(page, 'Invoice SS-101').getByText('pages 1–3 · $300.00')).toBeVisible();

  // Drag: the remittance slip onto "new invoice".
  const slip = page.getByRole('button', { name: /^Page 6\./ });
  const target = card(page, 'New invoice');
  await slip.scrollIntoViewIfNeeded();
  const from = (await slip.boundingBox())!;
  await page.mouse.move(from.x + from.width / 2, from.y + from.height / 2);
  await page.mouse.down();
  await page.mouse.move(from.x + 30, from.y + 10, { steps: 5 });
  const to = (await target.boundingBox())!;
  await page.mouse.move(to.x + to.width / 2, to.y + to.height / 2, { steps: 12 });
  await page.mouse.up();
  await expect(explain(page)).toHaveText('6 pages → 4 invoices · SS-101 runs 3 pages · nothing left over');
  // The side panel counts the cards as they are, and leaves creating to them.
  await expect(page.getByText('This file has 4 invoices.')).toBeVisible();
  await expect(page.getByRole('button', { name: /^Create bill/ })).toHaveCount(0);
  await expect(page.getByText('Create the bills from the invoice cards')).toBeVisible();
  await page.screenshot({ path: info.outputPath('split-view.png'), animations: 'disabled' });

  // One confirm, with the count and the total as read.
  await page.getByRole('button', { name: 'Create 4 bills' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('$465.50 as read, for 3 of 4')).toBeVisible();
  await dialog.getByRole('button', { name: 'Create 4 bills' }).click();

  // The server splits at once and reads and bills each part on its own;
  // the page follows along.
  const batch = page.getByRole('region', { name: 'Bills from this file' });
  await expect(batch.getByText('2 bills created · 1 stacked on existing bills · 1 needs you')).toBeVisible({ timeout: 45_000 });
  // Every page went somewhere, so the file itself left the inbox.
  await expect(page.getByText('Its pages were moved to other documents')).toBeVisible();
  await expect(page.getByRole('list', { name: 'Split into' }).getByRole('listitem')).toHaveCount(4);

  // It's still there when the page is opened again, each invoice's own result.
  await page.reload();
  await batch.getByText('Each invoice').click();
  const results = batch.getByRole('list', { name: 'Results' });
  await expect(results.getByRole('listitem').filter({ hasText: 'Invoice SS-101' })).toContainText('Bill created');
  await expect(results.getByRole('listitem').filter({ hasText: 'Invoice SS-102' })).toContainText('Bill created');
  await expect(results.getByRole('listitem').filter({ hasText: 'Invoice SS-103' })).toContainText('Stacked on an existing bill as a resend');
  await expect(results.getByRole('listitem').filter({ hasText: 'Invoice 4' })).toContainText('Needs you');
  await page.screenshot({ path: info.outputPath('split-view-done.png'), animations: 'disabled' });
});
