// Splitting a file of several invoices: the reader's suggested invoices on
// document review, ticking pages and creating a bill from them (the rest
// stays in To review), an agent's reading splitting the file straight away,
// and Undo, on a desktop and at phone width.
import { expect, test, type Page } from '@playwright/test';
import { agent } from './api';
import { pdfPages } from './pdf';
import { shown } from './viewer';

/**
 * Invoice 85273 on pages 1–2 ("Continued on next page"), invoice 85274 on
 * page 3 and the account statement on page 4, as the fake model reads them.
 */
async function reclamationFile(tag: string): Promise<{ id: string; filename: string }> {
  const filename = `reclaim-${tag}.pdf`;
  const bytes = pdfPages([
    ['American Reclamation', 'Invoice 85273', 'Continued on next page', tag],
    ['American Reclamation', 'Invoice 85273 continued', 'Total $8,034.89', tag],
    ['American Reclamation', 'Invoice 85274', 'Total $1,200.00', tag],
    ['American Reclamation', 'Account balance $26,923.52', tag],
  ]);
  const doc = await agent<{ id: string }>('document.upload', {
    filename,
    content_base64: bytes.toString('base64'),
  });
  return { id: doc.id, filename };
}

/** The one vendor the fake model's reading matches, made once. */
async function reclamationVendor(): Promise<void> {
  const { vendors } = await agent<{ vendors: { name: string }[] }>('vendor.list', {});
  if (!vendors.some((v) => v.name === 'American Reclamation')) {
    await agent('vendor.create', { name: 'American Reclamation' });
  }
}


const strip = (page: Page) => page.getByRole('region', { name: 'Pages' });
const attach = (page: Page, n: number) => strip(page).getByRole('checkbox', { name: `Include page ${n} in bill` });
const invoices = (page: Page) => page.getByRole('list', { name: 'Invoices in this file' });
const noSideScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);

test('the reader suggests the invoices, and a bill is made from the ticked pages', async ({ page }) => {
  await reclamationVendor();
  const file = await reclamationFile('desk');
  await page.goto(`/documents/${file.id}`);
  await expect(strip(page).getByText('4 pages')).toBeVisible();

  // A person reading it gets a suggestion, not a split.
  await page.getByRole('button', { name: 'Read it' }).click();
  await expect(page.getByText('This file has 2 invoices')).toBeVisible();
  await expect(invoices(page).getByText('Invoice 85273')).toBeVisible();
  await expect(invoices(page).getByText('pages 1–2 · $8,034.89')).toBeVisible();
  await expect(invoices(page).getByText('page 3 · $1,200.00')).toBeVisible();

  // The first invoice is ticked, with its number and total in the form.
  await expect(attach(page, 1)).toBeChecked();
  await expect(attach(page, 2)).toBeChecked();
  await expect(attach(page, 3)).not.toBeChecked();
  await expect(attach(page, 4)).not.toBeChecked();
  await expect(strip(page).getByText('2 of 4 selected')).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'Invoice number' })).toHaveValue('85273');
  await expect(page.getByRole('textbox', { name: 'Amount' })).toHaveValue('8034.89');

  // None, then All: nothing ticked can't be a bill; every page is the whole file.
  await strip(page).getByRole('button', { name: 'Select no pages' }).click();
  await expect(page.getByRole('button', { name: 'Create bill', exact: true })).toBeDisabled();
  await expect(page.getByText('Tick the pages for this bill.')).toBeVisible();
  await strip(page).getByRole('button', { name: 'Select all pages' }).click();
  await expect(page.getByRole('button', { name: 'Create bill', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Select Invoice 85273' }).click();
  await expect(strip(page).getByText('2 of 4 selected')).toBeVisible();

  await page.getByRole('button', { name: 'Create bill from 2 pages' }).click();
  await expect(page).toHaveURL(/\/bills\//);
  await expect.poll(() => shown(page)).toEqual([1, 2]);

  // The rest stays in To review, as the same document.
  await page.goto('/documents');
  const toReview = page.getByRole('region', { name: 'To review' });
  await expect(toReview.getByRole('link', { name: file.filename, exact: true })).toBeVisible();
  await expect(toReview.getByText('reclaim-desk (pages 1–2).pdf')).toHaveCount(0);
  await toReview.getByRole('link', { name: file.filename, exact: true }).click();
  await expect(strip(page).getByText('2 pages')).toBeVisible();
  await expect.poll(() => shown(page)).toEqual([3, 4]);
  await expect(page.getByRole('list', { name: 'Split into' }).getByText('reclaim-desk (pages 1–2).pdf')).toBeVisible();
  await expect(page.getByRole('list', { name: 'Page history' }).getByText('Robert split pages 1–2 into a new document')).toBeVisible();
});

test("an agent's reading splits the file at once, and Undo puts the pages back", async ({ page }) => {
  await reclamationVendor();
  const file = await reclamationFile('agent');
  const read = await agent<{ split: { to_read: string[] } }>('document.extract', { document_id: file.id });
  expect(read.split.to_read).toHaveLength(2);
  const [first, second] = read.split.to_read as [string, string];
  await agent('document.extract', { document_id: first });

  // The inbox shows where each part came from, with Undo.
  await page.goto('/documents');
  const toReview = page.getByRole('region', { name: 'To review' });
  await expect(toReview.getByText(`Split from ${file.filename}`)).toHaveCount(2);
  await toReview.getByRole('button', { name: 'Undo split of reclaim-agent (page 3).pdf' }).click();
  await expect(page.getByRole('region', { name: 'Notifications' }).getByText(`Pages put back in ${file.filename}`)).toBeVisible();
  await expect(toReview.getByText(`Split from ${file.filename}`)).toHaveCount(1);

  // The other part, read on its own: one invoice, with Undo on the page.
  await page.goto(`/documents/${first}`);
  await expect(page.getByText(`Split from ${file.filename}`)).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'Invoice number' })).toHaveValue('85273');
  await expect(page.getByText(/This file has/)).toHaveCount(0);
  await expect.poll(() => shown(page)).toEqual([1, 2]);
  await page.getByRole('button', { name: 'Undo', exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`/documents/${file.id}$`));

  // Whole again, with the agent's split and both undos on the record, and
  // the reader's suggestion back for a person to act on.
  await expect(strip(page).getByText('4 pages')).toBeVisible();
  await expect.poll(() => shown(page)).toEqual([1, 2, 3, 4]);
  const history = page.getByRole('list', { name: 'Page history' });
  await expect(history.getByText('claude-ap split pages 1–2 and page 3 into 2 new documents')).toBeVisible();
  await expect(history.getByText('Robert took back page 3 from reclaim-agent (page 3).pdf')).toBeVisible();
  await expect(history.getByText('Robert took back pages 1–2 from reclaim-agent (pages 1–2).pdf')).toBeVisible();
  await expect(page.getByText('This file has 2 invoices')).toBeVisible();
  const second_ = await agent<{ document: { status: string } }>('document.get', { document_id: second });
  expect(second_.document.status).toBe('split');
});

test('a bill from some pages on a phone', async ({ page }, info) => {
  await reclamationVendor();
  const file = await reclamationFile('phone');
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/documents/${file.id}`);
  await page.getByRole('button', { name: 'Read it' }).click();
  await expect(page.getByText('This file has 2 invoices')).toBeVisible();
  // The second invoice this time.
  await page.getByRole('button', { name: 'Select Invoice 85274' }).click();
  await expect(page.getByRole('textbox', { name: 'Invoice number' })).toHaveValue('85274');
  expect(await noSideScroll(page)).toBe(true);
  await page.screenshot({ path: info.outputPath('split-mobile-details.png'), animations: 'disabled' });

  // The ticks are on the pages, in the original document tab.
  await page.getByRole('radio', { name: 'Original document', exact: true }).click();
  await expect(attach(page, 3)).toBeChecked();
  await expect(attach(page, 1)).not.toBeChecked();
  await expect(strip(page).getByText('1 of 4 selected')).toBeVisible();
  expect(await noSideScroll(page)).toBe(true);
  await page.screenshot({ path: info.outputPath('split-mobile-pages.png'), animations: 'disabled' });

  await page.getByRole('radio', { name: 'Invoice details', exact: true }).click();
  await page.getByRole('button', { name: 'Create bill from 1 page' }).click();
  await expect(page).toHaveURL(/\/bills\//);
  await expect.poll(() => shown(page)).toEqual([3]);
  expect(await noSideScroll(page)).toBe(true);

  // Pages 1, 2 and 4 wait in To review.
  const rest = await agent<{ pages: { arrangement: { page: number }[] }; document: { status: string } }>(
    'document.get',
    { document_id: file.id },
  );
  expect(rest.document.status).toBe('inbox');
  expect(rest.pages.arrangement.map((p) => p.page)).toEqual([1, 2, 4]);
});
