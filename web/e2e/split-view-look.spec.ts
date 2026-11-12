// The split view at the shape of a real file: 40 pages, 38 invoices, the
// first running pages 1–3. Screenshots in both themes, desktop and phone,
// of the closed and opened stack, a hovered card, the header and the end
// of the grid with the "Not part of any bill" tray.
import { expect, test, type Page } from '@playwright/test';
import { agent } from './api';
import { pdfPages } from './pdf';

const VENDOR = 'Gila Freight Lines';

const card = (page: Page, name: string) => page.getByRole('region', { name, exact: true });

/** No card is narrower than its content or wider than the view; the page never scrolls sideways. */
async function fits(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  const over = await page.locator('.payvy-split-card').evaluateAll((cards) =>
    cards.filter((c) => c.scrollWidth > c.clientWidth + 1).length,
  );
  expect(over).toBe(0);
}

test('a 40-page file of 38 invoices looks right in both themes, on desktop and on a phone', async ({ page }, info) => {
  test.setTimeout(90_000);
  const tag = `split-look-${Date.now()}`;
  const pages: string[][] = [];
  for (let p = 1; p <= 40; p++) {
    const n = p <= 3 ? 1001 : 998 + p;
    pages.push([VENDOR, `Invoice GF-${n}${p > 1 && p <= 3 ? ' continued' : ''}`, `Page ${p}`, tag]);
  }
  const doc = await agent<{ id: string }>('document.upload', {
    filename: `${tag}.pdf`,
    content_base64: pdfPages(pages).toString('base64'),
  });
  await page.goto(`/inbox/${doc.id}`);
  await page.getByRole('button', { name: 'Read it' }).click();

  const first = card(page, 'Invoice GF-1001');
  const explain = page.locator('.payvy-split-explain');
  await expect(explain).toHaveText('40 pages → 38 invoices · GF-1001 runs 3 pages · nothing left over');
  await expect(first.getByRole('button', { name: 'Show the 3 pages of Invoice GF-1001 to move one' })).toHaveText('3 pages · show');

  // Cards in a row are the same height; the stack's label stays on one line.
  const rowHeights = async () => {
    const boxes = await page.locator('.payvy-split-card').evaluateAll((cs) =>
      cs.slice(0, 4).map((c) => { const r = c.getBoundingClientRect(); return { top: Math.round(r.top), h: Math.round(r.height) }; }),
    );
    const row = boxes.filter((b) => b.top === boxes[0]!.top);
    return new Set(row.map((b) => b.h)).size;
  };
  const oneLine = () => first.locator('.payvy-split-stack-open').evaluate((el) => el.getClientRects().length === 1 && el.getBoundingClientRect().height < 20);

  const view = page.locator('.payvy-split-view');
  const shoot = (name: string) => page.screenshot({ path: info.outputPath(`${name}.png`), animations: 'disabled' });

  for (const [size, viewport] of [['desktop', { width: 1440, height: 900 }], ['phone', { width: 390, height: 844 }]] as const) {
    await page.setViewportSize(viewport);
    if (size === 'phone') {
      await page.getByRole('radio', { name: 'Original document', exact: true }).click();
      await expect(first).toBeVisible();
    }
    for (const theme of ['light', 'dark'] as const) {
      await page.emulateMedia({ colorScheme: theme });
      await expect.poll(() => first.evaluate((el) => getComputedStyle(el).backgroundColor)).toBe(theme === 'light' ? 'rgb(255, 255, 255)' : 'rgb(32, 33, 39)');
      const at = `${size}-${theme}`;
      await view.evaluate((el) => el.scrollTo(0, 0));
      await expect(first.locator('.payvy-split-stack')).toBeVisible();
      // The first covers are in, and the mouse is off the cards.
      await page.mouse.move(viewport.width - 2, 2);
      await expect(first.locator('img')).toHaveCount(1);
      await expect(card(page, 'Invoice GF-1002').locator('img')).toHaveCount(1);
      expect(await oneLine()).toBe(true);
      expect(await rowHeights()).toBe(1);
      await fits(page);
      await shoot(`split-look-${at}-stack-closed`);
      await page.locator('.payvy-split-header').screenshot({ path: info.outputPath(`split-look-${at}-header.png`), animations: 'disabled' });
      if (size === 'desktop') {
        await page.locator('.payvy-split-mode').screenshot({ path: info.outputPath(`split-look-${at}-keys.png`), animations: 'disabled' });
      }

      await card(page, 'Invoice GF-1002').hover();
      await shoot(`split-look-${at}-card-hover`);

      await first.getByRole('button', { name: 'Show the 3 pages of Invoice GF-1001 to move one' }).click();
      await expect(first.getByRole('button', { name: /^Page \d\./ })).toHaveCount(3);
      expect(await rowHeights()).toBe(1);
      await fits(page);
      await page.mouse.move(viewport.width - 2, 2);
      await shoot(`split-look-${at}-stack-open`);

      const tray = card(page, 'Not part of any bill');
      await tray.scrollIntoViewIfNeeded();
      await expect(tray).toBeInViewport();
      await expect(card(page, 'Invoice GF-1038').locator('img')).toHaveCount(1);
      await shoot(`split-look-${at}-end`);

      // The keys switch views, and back again; the stack is closed once more.
      await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
      await page.keyboard.press('p');
      await expect(page.getByRole('radio', { name: 'Pages' })).toBeChecked();
      await page.keyboard.press('i');
      await expect(page.getByRole('radio', { name: 'Invoices' })).toBeChecked();
    }
  }
});
