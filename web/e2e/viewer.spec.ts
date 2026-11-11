// Document review shows one page at a time, as large as the panel allows:
// Previous and Next, ← → and J K (not while typing or in a dialog), the
// thumbnails hidden or shown as last chosen, ticking pages from the viewer,
// a phone, and a photo (one page, nothing to move through).
import { expect, test, type Page } from '@playwright/test';
import { agent } from './api';
import { pdfPages } from './pdf';
import { counter, currentPage } from './viewer';

/** A PDF of `n` pages no other test has. */
async function manyPages(tag: string, n: number): Promise<string> {
  const pages = Array.from({ length: n }, (_, i) => [`Page ${i + 1}`, `Acme ${tag}`, `Line ${i + 1} $10.00`]);
  const doc = await agent<{ id: string }>('document.upload', {
    filename: `acme-viewer-${tag}.pdf`,
    content_base64: pdfPages(pages).toString('base64'),
  });
  return doc.id;
}

const strip = (page: Page) => page.getByRole('region', { name: 'Pages' });
const toolbar = (page: Page) => page.locator('.payvy-viewer-toolbar');
const noSideScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);

test('one page at a time, turned with the buttons, arrows and J K', async ({ page }, info) => {
  const doc = await manyPages(`keys-${Date.now()}`, 8);
  await page.setViewportSize({ width: 1497, height: 1029 });
  await page.goto(`/documents/${doc}`);

  // Many pages: the thumbnails start hidden, leaving one slim row.
  await expect(counter(page)).toHaveText('Page 1 of 8');
  await expect(page.locator('.payvy-viewer-canvas [data-page]')).toHaveCount(1);
  const show = strip(page).getByRole('button', { name: 'Show thumbnails' });
  await expect(show).toHaveAttribute('aria-expanded', 'false');
  await expect(strip(page).getByText('8 of 8 selected')).toBeVisible();
  await expect(strip(page).getByRole('listitem')).toHaveCount(0);
  // The page starts near the top of the window and fits it.
  const canvas = page.locator('.payvy-viewer-canvas canvas');
  await expect(canvas).toBeVisible();
  const box = (await canvas.boundingBox())!;
  expect(box.y).toBeLessThanOrEqual(110);
  expect(box.y + box.height).toBeLessThanOrEqual(1029);
  await page.screenshot({ path: info.outputPath('viewer-desktop.png'), animations: 'disabled' });

  // The keys are shown, and the buttons name them too.
  await expect(toolbar(page).locator('.payvy-viewer-keys')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Next page' })).toHaveAttribute('aria-keyshortcuts', 'ArrowRight J');
  await expect(page.getByRole('button', { name: 'Previous page' })).toBeDisabled();

  await page.keyboard.press('ArrowRight');
  await expect(counter(page)).toHaveText('Page 2 of 8');
  expect(await currentPage(page)).toBe(2);
  await page.keyboard.press('j');
  await expect(counter(page)).toHaveText('Page 3 of 8');
  await page.keyboard.press('k');
  await expect(counter(page)).toHaveText('Page 2 of 8');
  await page.keyboard.press('ArrowLeft');
  await expect(counter(page)).toHaveText('Page 1 of 8');
  await page.keyboard.press('ArrowLeft');
  await expect(counter(page)).toHaveText('Page 1 of 8');
  // Not with a modifier held.
  await page.keyboard.press('Control+ArrowRight');
  await page.keyboard.press('Alt+j');
  await expect(counter(page)).toHaveText('Page 1 of 8');

  // Not while typing: the keys go to the field.
  const number = page.getByRole('textbox', { name: 'Invoice number' });
  await number.click();
  await page.keyboard.type('jk');
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowRight');
  await page.keyboard.type('j');
  await expect(number).toHaveValue('jkj');
  await expect(counter(page)).toHaveText('Page 1 of 8');

  // Not while a dialog is open.
  await page.getByRole('button', { name: 'Not a bill' }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.getByRole('button', { name: 'Next page' }).click();
  await expect(counter(page)).toHaveText('Page 2 of 8');
  await page.getByRole('button', { name: 'Previous page' }).click();
  await expect(counter(page)).toHaveText('Page 1 of 8');
  await page.getByRole('button', { name: 'Not a bill' }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.keyboard.press('j');
  await expect(counter(page)).toHaveText('Page 1 of 8');
  await page.keyboard.press('Escape');

  // Ticking pages while flipping through, without the thumbnails.
  await page.getByRole('button', { name: 'Next page' }).click();
  await expect(counter(page)).toHaveText('Page 2 of 8');
  const attach2 = toolbar(page).getByRole('checkbox', { name: 'Attach page 2' });
  await expect(attach2).toBeChecked();
  await attach2.uncheck();
  await expect(strip(page).getByText('7 of 8 selected')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Create bill from 7 pages' })).toBeVisible();
  await page.keyboard.press('j');
  await toolbar(page).getByRole('checkbox', { name: 'Attach page 3' }).uncheck();
  await expect(strip(page).getByText('6 of 8 selected')).toBeVisible();
  await page.keyboard.press('k');
  await expect(toolbar(page).getByRole('checkbox', { name: 'Attach page 2' })).not.toBeChecked();

  // Shown, the thumbnails agree, mark the page shown, and a click turns to a page.
  await show.click();
  await expect(strip(page).getByRole('button', { name: 'Hide thumbnails' })).toHaveAttribute('aria-expanded', 'true');
  await expect(strip(page).getByRole('checkbox', { name: 'Attach page 2' })).not.toBeChecked();
  await expect(strip(page).getByRole('checkbox', { name: 'Attach page 4' })).toBeChecked();
  await expect(strip(page).getByRole('button', { name: 'Show page 2' })).toHaveAttribute('aria-current', 'page');
  await strip(page).getByRole('button', { name: 'Show page 5' }).click();
  await expect(counter(page)).toHaveText('Page 5 of 8');
  await expect(strip(page).getByRole('button', { name: 'Show page 5' })).toHaveAttribute('aria-current', 'page');
  await expect(strip(page).getByRole('button', { name: 'Show page 2' })).not.toHaveAttribute('aria-current', 'page');
  // Ticking in the thumbnails shows in the viewer.
  await strip(page).getByRole('checkbox', { name: 'Attach page 5' }).uncheck();
  await expect(toolbar(page).getByRole('checkbox', { name: 'Attach page 5' })).not.toBeChecked();
  await page.screenshot({ path: info.outputPath('viewer-desktop-thumbnails.png'), animations: 'disabled' });

  // The choice is remembered in this browser, both ways.
  await page.reload();
  await expect(counter(page)).toHaveText('Page 1 of 8');
  await expect(strip(page).getByRole('button', { name: 'Hide thumbnails' })).toBeVisible();
  await expect(strip(page).getByRole('img', { name: 'Page 8' })).toBeAttached();
  await strip(page).getByRole('button', { name: 'Hide thumbnails' }).click();
  await page.reload();
  await expect(strip(page).getByRole('button', { name: 'Show thumbnails' })).toBeVisible();
  await expect(strip(page).getByRole('listitem')).toHaveCount(0);

  // Hiding the page shown moves the viewer to its neighbour.
  await expect(counter(page)).toHaveText('Page 1 of 8');
  await page.keyboard.press('j');
  await page.keyboard.press('j');
  await expect(counter(page)).toHaveText('Page 3 of 8');
  await strip(page).getByRole('button', { name: 'Show thumbnails' }).click();
  await strip(page).getByRole('button', { name: 'Hide page 3' }).click();
  await expect(counter(page)).toHaveText('Page 3 of 7');
  expect(await currentPage(page)).toBe(4);
});

test('on a phone the original turns with Previous and Next, without side scroll', async ({ page }, info) => {
  const doc = await manyPages(`phone-${Date.now()}`, 5);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/documents/${doc}`);
  await expect(page.getByRole('radiogroup', { name: 'Document view' })).toBeVisible();
  await page.getByRole('radio', { name: 'Original document', exact: true }).click();
  // A few pages: thumbnails shown at first.
  await expect(strip(page).getByRole('button', { name: 'Hide thumbnails' })).toBeVisible();
  await expect(counter(page)).toHaveText('Page 1 of 5');
  await page.getByRole('button', { name: 'Next page' }).click();
  await expect(counter(page)).toHaveText('Page 2 of 5');
  expect(await currentPage(page)).toBe(2);
  expect(await noSideScroll(page)).toBe(true);
  await page.screenshot({ path: info.outputPath('viewer-phone-thumbnails.png'), animations: 'disabled' });
  await strip(page).getByRole('button', { name: 'Hide thumbnails' }).click();
  await toolbar(page).getByRole('checkbox', { name: 'Attach page 2' }).uncheck();
  await expect(strip(page).getByText('4 of 5 selected')).toBeVisible();
  await page.getByRole('button', { name: 'Previous page' }).click();
  await expect(counter(page)).toHaveText('Page 1 of 5');
  expect(await noSideScroll(page)).toBe(true);
  // The page fills the width of the phone.
  const box = (await page.locator('.payvy-viewer-canvas canvas').boundingBox())!;
  expect(box.width).toBeGreaterThan(330);
  await page.screenshot({ path: info.outputPath('viewer-phone.png'), animations: 'disabled' });
  await page.getByRole('radio', { name: 'Invoice details', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Create bill from 4 pages' })).toBeAttached();
});

test('a photo is one page, with nothing to turn', async ({ page }) => {
  // A PNG drawn in the browser, unique to this run.
  const png = await page.evaluate((tag) => {
    const c = document.createElement('canvas');
    c.width = 600;
    c.height = 800;
    const ctx = c.getContext('2d')!;
    ctx.fillStyle = '#fff';
    ctx.fillRect(0, 0, 600, 800);
    ctx.fillStyle = '#000';
    ctx.font = '32px sans-serif';
    ctx.fillText(`Receipt ${tag}`, 40, 80);
    return c.toDataURL('image/png').split(',')[1]!;
  }, String(Date.now()));
  const doc = await agent<{ id: string }>('document.upload', { filename: `receipt-${Date.now()}.png`, content_base64: png });
  await page.goto(`/documents/${doc.id}`);
  await expect(page.locator('.payvy-viewer-canvas canvas')).toBeVisible();
  await expect(toolbar(page).getByText('1 page', { exact: true })).toBeVisible();
  await expect(page.getByRole('group', { name: 'Pages of the document' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Next page' })).toHaveCount(0);
  await page.keyboard.press('j');
  await expect(page.locator('.payvy-viewer-canvas [data-page="1"]')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Fit width' })).toBeEnabled();
});
