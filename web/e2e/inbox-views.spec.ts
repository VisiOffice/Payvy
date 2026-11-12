// The inbox shows what was read from each file, as a table or a board, and
// T and B switch between them.
import { expect, test } from '@playwright/test';
import { agent, vendor } from './api';
import { pdf, pdfPages } from './pdf';

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

test('side by side: the list beside the review, J/K move it, and making a bill moves on to the next', async ({ page }, info) => {
  test.setTimeout(60_000);
  const tag = `side-${Date.now()}`;
  for (const n of [1, 2]) {
    const doc = (await agent<{ id: string }>('document.upload', {
      filename: `${tag}-${n}.pdf`,
      content_base64: pdf(['Sonoran Pest Control', 'Invoice SR-7781', 'Total 185.00', `${tag} ${n}`]).toString('base64'),
    })).id;
    await agent('document.extract', { document_id: doc });
  }
  for (const width of [1440, 2560]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.goto('/inbox');
    // Only this test's files: other tests leave theirs in the inbox.
    await page.getByRole('textbox', { name: 'Search documents' }).fill(tag);
    await page.getByRole('heading', { name: 'Inbox', level: 1 }).click();
    await page.keyboard.press('s');
    const list = page.getByRole('list', { name: 'Documents' });
    await expect(list).toBeVisible();
    const items = list.getByRole('button');
    await expect(items.first()).toHaveAttribute('aria-current', 'true');
    await expect(page.locator('.payvy-inbox-side-pane').getByLabel('Invoice number')).toHaveValue('SR-7781');
    await page.screenshot({ path: info.outputPath(`side-${width}.png`), animations: 'disabled' });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  }

  // J moves the list and the review follows.
  const list = page.getByRole('list', { name: 'Documents' });
  const items = list.getByRole('button');
  const second = await items.nth(1).locator('.payvy-inbox-side-file').textContent();
  await page.keyboard.press('j');
  await expect(items.nth(1)).toHaveAttribute('aria-current', 'true');
  await expect(page.locator('.payvy-inbox-side-pane .payvy-review-filename')).toHaveText(second!);
  await page.keyboard.press('k');
  await expect(items.nth(0)).toHaveAttribute('aria-current', 'true');
  // A file of several invoices: I, P and T switch its view, and T stays with it.
  const multi = `${tag}-multi`;
  const many = (await agent<{ id: string }>('document.upload', {
    filename: `${multi}.pdf`,
    // The fake reader's six-page Saguaro file: SS-101 on pages 1–3, then SS-102 and SS-103.
    content_base64: pdfPages([
      ['Saguaro Storage Co', 'Invoice SS-101', 'Continued', multi],
      ['Saguaro Storage Co', 'Invoice SS-101 continued', multi],
      ['Saguaro Storage Co', 'Invoice SS-101 total $300.00', multi],
      ['Saguaro Storage Co', 'Invoice SS-102 total $120.00', multi],
      ['Saguaro Storage Co', 'Invoice SS-103 total $45.50', multi],
      ['Saguaro Storage Co', 'Remittance slip', multi],
    ]).toString('base64'),
  })).id;
  expect(many).toBeTruthy();
  await page.reload();
  await page.getByRole('textbox', { name: 'Search documents' }).fill(multi);
  await page.getByRole('heading', { name: 'Inbox', level: 1 }).click();
  // A person reads it here (an agent's reading would split it into separate files at once).
  await page.locator('.payvy-inbox-side-pane').getByRole('button', { name: 'Read it' }).click();
  await expect(page.getByRole('radio', { name: 'Invoices' })).toBeChecked();
  for (const [key, name] of [['p', 'Pages'], ['t', 'Table'], ['i', 'Invoices']] as const) {
    await page.keyboard.press(key);
    await expect(page.getByRole('radio', { name, exact: true }).last()).toBeChecked();
    await expect(page.getByRole('list', { name: 'Documents' })).toBeVisible();
  }

  // Making a bill keeps you in the inbox and puts the next file in front of you.
  await vendor('Dropped Hauling').catch(() => undefined);
  const made = `${tag}-bill`;
  for (const n of [1, 2]) {
    const doc = (await agent<{ id: string }>('document.upload', {
      filename: `${made}-${n}.pdf`,
      content_base64: pdf(['Dropped Hauling', 'Invoice DR-4410', 'Date 11/03/2026', 'Total 42.00', `${made} ${n}`]).toString('base64'),
    })).id;
    await agent('document.extract', { document_id: doc });
  }
  await page.reload();
  await page.getByRole('textbox', { name: 'Search documents' }).fill(made);
  await page.getByRole('heading', { name: 'Inbox', level: 1 }).click();
  const pane = page.locator('.payvy-inbox-side-pane');
  await expect(pane.locator('.payvy-review-filename')).toHaveText(`${made}-1.pdf`);
  // Choose the vendor in the form: other tests may have added one of the same name.
  const trigger = pane.locator('[data-copy-field="vendor_name"] button').first();
  if (!(await pane.getByRole('button', { name: 'Create bill', exact: true }).isEnabled())) {
    await trigger.click();
    await page.getByRole('option', { name: 'Dropped Hauling' }).first().click();
  }
  await pane.getByRole('button', { name: 'Create bill', exact: true }).click();
  await expect(page).toHaveURL(/\/inbox$/);
  await expect(pane.locator('.payvy-review-filename')).toHaveText(`${made}-2.pdf`);
  await expect(list.getByRole('button')).toHaveCount(1);
  await page.getByRole('textbox', { name: 'Search documents' }).fill('');

  // The view is remembered; T goes back to the table.
  await page.reload();
  await expect(page.getByRole('list', { name: 'Documents' })).toBeVisible();
  await page.keyboard.press('t');
  await expect(page.locator('.payvy-inbox-table')).toBeVisible();
});
