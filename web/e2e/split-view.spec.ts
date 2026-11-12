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

  // An invoice of several pages is one stack until opened to move a page.
  await expect(card(page, 'Invoice SS-101').getByRole('button', { name: /^Page \d\./ })).toHaveCount(0);
  await card(page, 'Invoice SS-101').getByRole('button', { name: 'Show the 3 pages of Invoice SS-101 to move one' }).click();
  await expect(card(page, 'Invoice SS-101').getByRole('button', { name: /^Page \d\./ })).toHaveCount(3);

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

  // A click anywhere on a card reviews that invoice page by page; I brings the cards back.
  await card(page, 'Invoice SS-102').getByText('page 4 · $120.00').click();
  await expect(page.getByRole('radio', { name: 'Pages' })).toBeChecked();
  await expect(page.getByLabel('Invoice number')).toHaveValue('SS-102');
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.keyboard.press('i');
  await expect(page.getByRole('radio', { name: 'Invoices' })).toBeChecked();

  // T lists the same invoices, one row each. A row opens that invoice; I returns to the cards.
  await page.keyboard.press('t');
  await expect(page.getByRole('radio', { name: 'Table' })).toBeChecked();
  const row = (name: string) => page.locator('.payvy-split-table tbody tr').filter({ hasText: name });
  await expect(page.locator('.payvy-split-table tbody tr')).toHaveCount(4);
  await expect(row('Invoice SS-101')).toContainText('pages 1–3');
  await expect(row('Invoice SS-101')).toContainText('$300.00');
  await expect(row('Invoice SS-102')).toContainText('page 4');
  await expect(row('Invoice SS-102')).toContainText('$120.00');
  await expect(row('Invoice SS-102').getByRole('button', { name: 'Review Invoice SS-102' })).toBeVisible();
  await expect(row('Invoice SS-103')).toContainText('page 5');
  await expect(row('Invoice SS-103')).toContainText('$45.50');
  await expect(row('Invoice 4')).toContainText('page 6');
  await expect(row('Invoice 4')).toContainText('read when created');
  await row('Invoice SS-103').getByText('page 5').click();
  await expect(page.getByRole('radio', { name: 'Pages' })).toBeChecked();
  await expect(page.getByLabel('Invoice number')).toHaveValue('SS-103');
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.keyboard.press('i');
  await expect(page.getByRole('radio', { name: 'Invoices' })).toBeChecked();
  await expect(card(page, 'Invoice SS-101')).toBeVisible();

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
