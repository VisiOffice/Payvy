// Long lists use the whole width: on a wide screen the text columns take the
// extra room and the numbers stay their size. Settings stay narrow.
import { expect, test, type Page } from '@playwright/test';
import { agent, bill, vendor } from './api';
import { pdf } from './pdf';

/** The page never scrolls sideways. */
const noSideScroll = (page: Page) =>
  page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);

test('inbox, bills and vendors fill a wide screen; settings stay narrow', async ({ page }, info) => {
  test.setTimeout(60_000);
  const tag = `wide-${Date.now()}`;
  const v = await vendor(`Wide Hauling ${tag}`);
  await bill({ vendor_id: v, invoice_number: `WH-${tag}`, invoice_date: '2026-11-03', amount_cents: 123_456 });
  const doc = await agent<{ id: string }>('document.upload', {
    filename: `${tag}.pdf`,
    content_base64: pdf(['Sonoran Pest Control', 'Invoice SR-7781', 'Total 185.00', tag]).toString('base64'),
  });
  await agent('document.extract', { document_id: doc.id });

  for (const width of [1440, 2560]) {
    await page.setViewportSize({ width, height: 1000 });
    for (const [path, heading, table] of [
      ['/inbox', 'Inbox', '.payvy-inbox-table'],
      ['/bills', 'All bills', '.payvy-table table'],
      ['/vendors', 'Vendors', 'table'],
    ] as const) {
      await page.goto(path);
      await expect(page.getByRole('heading', { name: heading, level: 1 })).toBeVisible();
      const t = page.locator(table).first();
      await expect(t).toBeVisible();
      await page.screenshot({ path: info.outputPath(`${path.slice(1)}-${width}.png`) });
      expect(await noSideScroll(page)).toBe(true);
      // Past the old 1240px frame on a wide screen.
      if (width === 2560) expect((await t.boundingBox())!.width).toBeGreaterThan(1600);
    }
    // The amount column keeps its size however wide the screen is.
    await page.goto('/bills');
    const amount = page.getByRole('columnheader', { name: 'Amount' });
    expect((await amount.boundingBox())!.width).toBeLessThan(200);

    await page.goto('/settings/preferences');
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
    expect((await page.locator('.payvy-page').boundingBox())!.width).toBeLessThanOrEqual(760);
  }
});
