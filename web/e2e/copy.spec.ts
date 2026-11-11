// Click-to-copy on document review, as on Bill.com: hovering text on the page
// highlights it, a click copies it, and clicking a field pastes it, read as
// that field reads it (amounts, dates). Or the other way round: focus a field,
// then click text on the page. Typing and Ctrl/⌘+V are left alone.
import { expect, test, type Page } from '@playwright/test';
import { agent } from './api';
import { pdfPages } from './pdf';
import { counter, currentPage } from './viewer';
import { parseAmount, parseDate } from '../src/viewer/match';
import { runs } from '../src/viewer/runs';
import { searchFor } from '../src/documents/clickToCopy';

const SHOTS = process.env.PAYVY_COPY_SHOTS;

/** A two-page invoice with real text, no other test has. */
async function invoice(tag: string): Promise<string> {
  const doc = await agent<{ id: string }>('document.upload', {
    filename: `american-reclamation-${tag}.pdf`,
    content_base64: pdfPages([
      [
        'AMERICAN RECLAMATION INC',
        'Invoice #: 85273',
        'Invoice date: 6/15/26',
        'Due: June 30, 2026',
        'Total due: $8,034.89',
        'PO QR-7781',
        `Ref ${tag}`,
      ],
      ['Remittance', 'Amount enclosed: $1,250.00', 'Paid on 07/01/2026', `Ref ${tag}`],
    ]).toString('base64'),
  });
  return doc.id;
}

const run = (page: Page, text: string) => page.locator(`.payvy-viewer-canvas .payvy-copy-run[data-run="${text}"]`);
const chip = (page: Page) => page.locator('.payvy-copy-chip');
const status = (page: Page) => page.locator('[role="status"][aria-live="polite"].payvy-sr-only');
const field = (page: Page, name: string) => page.getByRole('textbox', { name });
const vendorTrigger = (page: Page) => page.locator('[data-copy-field="vendor_name"] button').first();

async function shot(page: Page, name: string, info: { outputPath: (n: string) => string }) {
  await page.screenshot({ path: info.outputPath(name), animations: 'disabled' });
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/${name}`, animations: 'disabled' });
}

test('the helpers read amounts, dates and runs as printed', () => {
  expect(parseAmount('$8,034.89')).toBe('8034.89');
  expect(parseAmount('USD 1,200')).toBe('1200.00');
  expect(parseAmount('8034.9')).toBe('8034.90');
  expect(parseAmount('Total')).toBeNull();
  expect(parseDate('6/15/26')).toBe('2026-06-15');
  expect(parseDate('06/15/2026')).toBe('2026-06-15');
  expect(parseDate('06-15-2026')).toBe('2026-06-15');
  expect(parseDate('June 15, 2026')).toBe('2026-06-15');
  expect(parseDate('Jun. 15, 2026')).toBe('2026-06-15');
  expect(parseDate('15 June 2026')).toBe('2026-06-15');
  expect(parseDate('2026-06-15')).toBe('2026-06-15');
  expect(parseDate('2/30/26')).toBeNull();
  expect(parseDate('85273')).toBeNull();
  // One item per line, as the generated PDFs give: a label and its value part.
  const line = (str: string, y: number) => ({ str, x: 72, y, w: str.length * 6.5, h: 12 });
  expect(runs([line('Invoice #: 85273', 100), line('AMERICAN RECLAMATION INC', 130)]).map((r) => r.str)).toEqual([
    'Invoice #:',
    '85273',
    'AMERICAN RECLAMATION INC',
  ]);
  // Columns apart, and a word split into glyphs.
  const glyphs = [...'$12.00'].map((c, i) => ({ str: c, x: 300 + i * 6, y: 200, w: 6, h: 12 }));
  expect(runs([{ str: 'Service call', x: 72, y: 200, w: 70, h: 12 }, ...glyphs]).map((r) => r.str)).toEqual([
    'Service call',
    '$12.00',
  ]);
  expect(searchFor('AMERICAN RECLAMATION INC', ['American Reclamation, Inc.', 'Mesa Electric'])).toBe('AMERICAN RECLAMATION');
  expect(searchFor('Nobody Here', ['Mesa Electric'])).toBe('Nobody Here');
});

test('click text to copy it, then click a field to paste it', async ({ page, context }, info) => {
  const tag = `${Date.now()}`;
  const doc = await invoice(tag);
  const vendorName = `American Reclamation ${tag}`;
  await agent('vendor.create', { name: vendorName });
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto(`/documents/${doc}`);
  await expect(counter(page)).toHaveText('Page 1 of 2');

  // Hover: the run lights up with a tip.
  await run(page, '85273').hover();
  await expect(run(page, '85273')).toHaveAttribute('data-hover', '');
  await expect(page.locator('.payvy-copy-tip')).toHaveText('Click to copy: 85273');
  await shot(page, 'copy-hover.png', info);

  // Click copies, in the app and to the system clipboard.
  await run(page, '85273').click();
  await expect(chip(page)).toContainText('Copied “85273”');
  await expect(chip(page)).toContainText('Click a field to paste');
  await expect(status(page)).toHaveText(/Copied 85273/);
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('85273');

  // Clicking a field pastes it.
  await field(page, 'Invoice number').click();
  await expect(field(page, 'Invoice number')).toHaveValue('85273');
  await expect(chip(page)).toContainText('Filled Invoice number with 85273');
  await expect(status(page)).toHaveText(/Filled Invoice number with 85273/);
  await expect(page.locator('[data-copy-field="invoice_number"][data-flash]')).toBeAttached();
  await shot(page, 'copy-after-paste.png', info);

  // Amounts lose the dollar sign and commas.
  await run(page, '$8,034.89').click();
  await field(page, 'Amount').click();
  await expect(field(page, 'Amount')).toHaveValue('8034.89');
  await expect(page.locator('.payvy-invoice-amount')).toHaveText('$8,034.89');

  // Dates are read month first, and named months too.
  await run(page, '6/15/26').click();
  await page.locator('[data-copy-field="invoice_date"]').click();
  await expect(chip(page)).toContainText('Filled Invoice date with Jun 15, 2026');
  await expect(page.getByRole('combobox', { name: 'Invoice date' })).toHaveValue('Jun 15, 2026');
  await run(page, 'June 30, 2026').click();
  await page.locator('[data-copy-field="due_date"]').click();
  await expect(chip(page)).toContainText('Filled Due with Jun 30, 2026');
  await expect(page.getByRole('combobox', { name: 'Due' })).toHaveValue('Jun 30, 2026');
  await expect(page.getByText("No due date: the vendor's payment terms decide it.")).toHaveCount(0);

  // Text that isn't a date stays copied for another field.
  await run(page, 'PO QR-7781').click();
  await page.locator('[data-copy-field="due_date"]').click();
  await expect(chip(page)).toContainText('isn\'t a date');
  await expect(status(page)).toHaveText(/“PO QR-7781” isn't a date/);
  await field(page, 'Memo').click();
  await expect(field(page, 'Memo')).toHaveValue('PO QR-7781');

  // Esc lets go of the copied text: a click on a field then does nothing.
  await run(page, 'AMERICAN RECLAMATION INC').click();
  await expect(chip(page)).toContainText('Copied “AMERICAN RECLAMATION INC”');
  await page.keyboard.press('Escape');
  await expect(chip(page)).toHaveCount(0);
  await field(page, 'Invoice number').click();
  await expect(field(page, 'Invoice number')).toHaveValue('85273');

  // Shift-click copies only the word under the pointer (Esc first: the field waits for the page).
  await expect(page.locator('[data-copy-field="invoice_number"] .payvy-copy-hint')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.locator('.payvy-copy-hint')).toHaveCount(0);
  const vendorRun = run(page, 'AMERICAN RECLAMATION INC');
  const box = (await vendorRun.boundingBox())!;
  await page.keyboard.down('Shift');
  await page.mouse.click(box.x + box.width - 4, box.y + box.height / 2);
  await page.keyboard.up('Shift');
  await expect(chip(page)).toContainText('Copied “INC”');
  await page.keyboard.press('Escape');

  // The vendor list opens searched for the name; the person picks.
  await vendorRun.click();
  await vendorTrigger(page).click();
  const search = page.getByRole('combobox', { name: 'Search options' });
  await expect(search).toHaveValue('AMERICAN RECLAMATION');
  await expect(page.getByRole('option', { name: vendorName })).toBeVisible();
  await expect(vendorTrigger(page)).toContainText('Choose a vendor');
  await page.getByRole('option', { name: vendorName }).click();
  await expect(vendorTrigger(page)).toContainText(vendorName);
  // Chosen, the vendor isn't waiting for text from the page.
  await expect(page.locator('.payvy-copy-hint')).toHaveCount(0);

  // An ordinary paste still works: the system clipboard has the last copy.
  await run(page, '85273').click();
  await page.keyboard.press('Escape');
  await field(page, 'Memo').fill('');
  await field(page, 'Memo').focus();
  await page.keyboard.press('ControlOrMeta+V');
  await expect(field(page, 'Memo')).toHaveValue('85273');
});

test('focus a field, then click text on the page; on page 2, and turned', async ({ page }, info) => {
  const tag = `${Date.now()}`;
  const doc = await invoice(tag);
  await page.goto(`/documents/${doc}`);
  await expect(counter(page)).toHaveText('Page 1 of 2');
  await expect(run(page, '85273')).toBeAttached();

  // The focused field says it takes the next click on the page.
  const number = field(page, 'Invoice number');
  await number.click();
  const hint = page.locator('[data-copy-field="invoice_number"] .payvy-copy-hint');
  await expect(hint).toHaveText('Click text on the document to fill');
  await run(page, '85273').click();
  await expect(number).toHaveValue('85273');
  await expect(hint).toHaveCount(0);
  // Once: the next click copies.
  await run(page, 'PO QR-7781').click();
  await expect(chip(page)).toContainText('Copied “PO QR-7781”');
  await page.keyboard.press('Escape');

  // Typing in a field means it isn't waiting for the page.
  await field(page, 'Memo').click();
  await expect(page.locator('[data-copy-field="po_number"] .payvy-copy-hint')).toBeVisible();
  await page.keyboard.type('typed');
  await expect(page.locator('[data-copy-field="po_number"] .payvy-copy-hint')).toHaveCount(0);
  await run(page, 'PO QR-7781').click();
  await expect(field(page, 'Memo')).toHaveValue('typed');
  await expect(chip(page)).toContainText('Copied “PO QR-7781”');
  await page.keyboard.press('Escape');

  // The keys still turn the page after a copy; page 2's text is there.
  await run(page, '85273').click();
  await page.keyboard.press('j');
  await expect(counter(page)).toHaveText('Page 2 of 2');
  await expect(run(page, '85273')).toHaveCount(0);
  await page.keyboard.press('Escape');
  await field(page, 'Amount').click();
  await run(page, '$1,250.00').click();
  await expect(field(page, 'Amount')).toHaveValue('1250.00');
  await page.keyboard.press('k');
  await expect(counter(page)).toHaveText('Page 1 of 2');
  await page.keyboard.press('ArrowRight');
  await expect(counter(page)).toHaveText('Page 2 of 2');

  // Turned a quarter: the text boxes turn with the page.
  const d = await agent<{ pages: { version: number } }>('document.get', { document_id: doc });
  await agent('document.arrange', {
    document_id: doc,
    expected_version: d.pages.version,
    pages: [{ page: 1 }, { page: 2, rotation: 90 }],
  });
  await page.reload();
  await expect(counter(page)).toHaveText('Page 1 of 2');
  await page.keyboard.press('j');
  await expect(counter(page)).toHaveText('Page 2 of 2');
  expect(await currentPage(page)).toBe(2);
  await expect(page.locator('.payvy-viewer-canvas [data-rotation="90"]')).toBeVisible();
  const turned = run(page, 'Paid on 07/01/2026');
  const box = (await turned.boundingBox())!;
  expect(box.height).toBeGreaterThan(box.width);
  await turned.hover();
  await expect(page.locator('.payvy-copy-tip')).toHaveText('Click to copy: Paid on 07/01/2026');
  await shot(page, 'copy-rotated.png', info);
  await turned.click();
  await page.locator('[data-copy-field="due_date"]').click();
  await expect(chip(page)).toContainText('“Paid on 07/01/2026” isn\'t a date');
  await page.keyboard.press('Escape');
  // Zoomed in, the boxes scale with the page.
  const before = (await turned.boundingBox())!;
  await page.getByRole('button', { name: 'Zoom in' }).click();
  await page.getByRole('button', { name: 'Zoom in' }).click();
  await expect.poll(async () => (await turned.boundingBox())!.height).toBeGreaterThan(before.height * 1.1);
  await page.getByRole('combobox', { name: 'Due' }).click();
  await expect(page.locator('[data-copy-field="due_date"] .payvy-copy-hint')).toBeVisible();
  // A word of it, with Shift.
  const zoomed = (await turned.boundingBox())!;
  await page.keyboard.down('Shift');
  await page.mouse.click(zoomed.x + zoomed.width / 2, zoomed.y + zoomed.height - 6);
  await page.keyboard.up('Shift');
  await expect(chip(page)).toContainText('Filled Due with Jul 1, 2026');
});

test('a filed document still copies, and pastes nowhere', async ({ page, context }) => {
  const tag = `${Date.now()}`;
  const doc = await invoice(tag);
  const vendorId = (await agent<{ id: string }>('vendor.create', { name: `Filed Reclamation ${tag}` })).id;
  await agent('bill.create', {
    vendor_id: vendorId,
    invoice_number: `85273-${tag}`,
    invoice_date: '2026-06-15',
    amount_cents: 803489,
    document_id: doc,
  });
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto(`/documents/${doc}`);
  await expect(page.getByText('This document is a bill')).toBeVisible();
  await run(page, '$8,034.89').click();
  await expect(chip(page)).toContainText('Copied “$8,034.89”');
  await expect(chip(page)).not.toContainText('Click a field');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('$8,034.89');
});

test.describe('on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test('tap text to copy it, then tap a field', async ({ page }, info) => {
    const tag = `${Date.now()}`;
    const doc = await invoice(tag);
    await page.goto(`/documents/${doc}`);
    await page.getByRole('radio', { name: 'Original document' }).tap();
    await expect(counter(page)).toHaveText('Page 1 of 2');
    await run(page, '$8,034.89').tap();
    await expect(chip(page)).toContainText('Copied “$8,034.89”');
    await expect(page.locator('.payvy-copy-tip')).toHaveCount(0);
    await shot(page, 'copy-phone-copied.png', info);
    await page.getByRole('radio', { name: 'Invoice details' }).tap();
    await field(page, 'Amount').tap();
    await expect(field(page, 'Amount')).toHaveValue('8034.89');
    await expect(chip(page)).toContainText('Filled Amount with 8034.89');
    await shot(page, 'copy-phone-pasted.png', info);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  });
});
