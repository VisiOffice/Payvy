// The inbox shows what was read from each file, as a table or a board, and
// T and B switch between them.
import { expect, test } from '@playwright/test';
import { agent } from './api';
import { pdf } from './pdf';

test('the inbox shows what was read, as a table or a board, switched with T and B', async ({ page }, info) => {
  // A file of its own (the server refuses the same file twice), read by the
  // fake model as Sonoran Pest Control's SR-7781 for $185.00.
  const doc = (
    await agent<{ id: string }>('document.upload', {
      filename: 'inbox-views-sr-7781.pdf',
      content_base64: pdf(['Sonoran Pest Control', 'Invoice SR-7781', 'Total 185.00', 'Inbox views']).toString('base64'),
    })
  ).id;
  await agent('document.extract', { document_id: doc });

  await page.goto('/inbox');
  await expect(page.getByRole('heading', { name: 'Inbox', level: 1 })).toBeVisible();
  await page.evaluate(() => localStorage.removeItem('payvy.inbox.view'));
  await page.reload();

  // The table: vendor, invoice, amount and the first page, not just a filename.
  const toReview = page.getByRole('region', { name: 'To review' });
  const row = toReview.getByRole('row').filter({ has: page.getByRole('link', { name: 'inbox-views-sr-7781.pdf', exact: true }) });
  await expect(row.getByText('Sonoran Pest Control')).toBeVisible();
  await expect(row.getByText('1 invoice')).toBeVisible();
  await expect(row.getByText('SR-7781', { exact: true })).toBeVisible();
  await expect(row.getByText('$185.00')).toBeVisible();
  // The table is text only; the first page shows on the board.
  await expect(row.locator('img')).toHaveCount(0);

  await page.screenshot({ path: info.outputPath('inbox-table.png'), fullPage: true, animations: 'disabled' });

  // Search finds it by invoice number, not only by filename.
  await page.getByLabel('Search documents').fill('sr-7781');
  await expect(row).toBeVisible();
  await page.getByLabel('Search documents').fill('');

  // B: the board, by what each file needs next. Typing in a field isn't a shortcut.
  await page.getByLabel('Search documents').press('b');
  await expect(page.getByRole('region', { name: 'Needs you' })).toHaveCount(0);
  await page.getByLabel('Search documents').fill('');
  await page.locator('body').click({ position: { x: 5, y: 5 } });
  await page.keyboard.press('b');
  for (const lane of ['Reading', 'Needs you', 'Ready to create', 'Became bills']) {
    await expect(page.getByRole('region', { name: lane })).toBeVisible();
  }
  const card = page.getByRole('listitem').filter({ has: page.getByRole('link', { name: 'inbox-views-sr-7781.pdf', exact: true }) });
  await expect(card.getByText('$185.00')).toBeVisible();
  // Its first page loads as the card scrolls into view.
  await card.scrollIntoViewIfNeeded();
  const cover = card.locator('img');
  await expect.poll(() => cover.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth)).toBeGreaterThan(0);
  await expect(page.getByRole('radio', { name: 'Board' })).toBeChecked();
  await page.screenshot({ path: info.outputPath('inbox-board.png'), fullPage: true, animations: 'disabled' });

  // Remembered, and T goes back to the table.
  await page.reload();
  await expect(page.getByRole('region', { name: 'Needs you' })).toBeVisible();
  await page.keyboard.press('t');
  await expect(page.getByRole('region', { name: 'To review' })).toBeVisible();
  await expect(page.getByRole('radio', { name: 'Table' })).toBeChecked();

  // F, N and R pick Filed, Not bills and To review; from the board they bring the table back.
  await page.keyboard.press('f');
  await expect(page.getByRole('radio', { name: 'Filed' })).toBeChecked();
  await page.keyboard.press('n');
  await expect(page.getByRole('radio', { name: 'Not bills' })).toBeChecked();
  await page.keyboard.press('b');
  await page.keyboard.press('r');
  await expect(page.getByRole('radio', { name: 'Table' })).toBeChecked();
  await expect(page.getByRole('radio', { name: 'To review' })).toBeChecked();
  await expect(row).toBeVisible();
  await page.screenshot({ path: info.outputPath('inbox-keys.png'), animations: 'disabled' });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);

  // The inbox used to be /documents; those links still open the same place.
  await page.goto(`/documents/${doc}`);
  await expect(page).toHaveURL(`/inbox/${doc}`);
  await page.goto('/documents');
  await expect(page).toHaveURL('/inbox');
});

test('J and K, or the arrows, move through the inbox table; Enter opens the row', async ({ page }, info) => {
  const tag = `keys-${Date.now()}`;
  for (const n of [1, 2]) {
    const doc = (await agent<{ id: string }>('document.upload', {
      filename: `${tag}-${n}.pdf`,
      content_base64: pdf(['Sonoran Pest Control', 'Invoice SR-7781', 'Total 185.00', `${tag} ${n}`]).toString('base64'),
    })).id;
    await agent('document.extract', { document_id: doc });
  }
  await page.goto('/inbox');
  await page.evaluate(() => localStorage.removeItem('payvy.inbox.view'));
  await page.reload();
  await expect(page.getByText('or drop files anywhere on this page')).toBeVisible();

  const rows = page.locator('.payvy-inbox-table tbody tr');
  await expect(rows.nth(1)).toBeVisible();
  const current = page.locator('.payvy-inbox-table tbody tr[aria-current="true"]');
  await expect(current).toHaveCount(0);

  await page.keyboard.press('j');
  await expect(rows.nth(0)).toHaveAttribute('aria-current', 'true');
  await page.keyboard.press('ArrowDown');
  await expect(rows.nth(1)).toHaveAttribute('aria-current', 'true');
  await expect(current).toHaveCount(1);
  await page.keyboard.press('k');
  await expect(rows.nth(0)).toHaveAttribute('aria-current', 'true');
  await page.keyboard.press('ArrowDown');
  await page.screenshot({ path: info.outputPath('inbox-keys.png'), animations: 'disabled' });

  const href = await rows.nth(1).getByRole('link').first().getAttribute('href');
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(new RegExp(`${href}$`));
});
