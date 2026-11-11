// Reading what the document viewer shows. On document review it shows one
// page at a time; on the bill page, every page in a column.
import { expect, type Page } from '@playwright/test';

/** "Page 2 of 4", announced as the page turns. */
export const counter = (page: Page) => page.getByRole('group', { name: 'Pages of the document' }).getByText(/^Page \d+ of \d+$/);

/** The original's page number of the page the viewer shows now. */
export const currentPage = (page: Page) =>
  page.locator('.payvy-viewer-canvas [data-page]').first().getAttribute('data-page').then(Number);

/**
 * A page turn is instant. A turn that doesn't land means the pages changed
 * under the walk (a save arrived), so fail fast and let expect.poll read again.
 */
const TURN = 1000;

/**
 * The original's page numbers in the order the viewer shows them. One page at
 * a time, it turns back to the first with Previous, then reads each with Next.
 * Use it inside expect.poll: a read during a save can fail and is retried.
 */
export async function shown(page: Page): Promise<number[]> {
  const pages = page.locator('.payvy-viewer-canvas [data-page]');
  if ((await page.locator('.payvy-viewer-canvas[data-paged]').count()) === 0) {
    return pages.evaluateAll((els) => els.map((e) => Number(e.getAttribute('data-page'))));
  }
  if ((await pages.count()) !== 1) return [];
  const nav = page.getByRole('group', { name: 'Pages of the document' });
  if ((await nav.count()) === 0) return [await currentPage(page)];
  const numbers = ((await counter(page).textContent({ timeout: 1000 }).catch(() => null)) ?? '').match(/\d+/g);
  if (!numbers) return [];
  const [at, of] = numbers.map(Number) as [number, number];
  for (let i = at; i > 1; i--) {
    await nav.getByRole('button', { name: 'Previous page' }).click();
    await expect(counter(page)).toHaveText(`Page ${i - 1} of ${of}`, { timeout: TURN });
  }
  const order: number[] = [];
  for (let i = 1; i <= of; i++) {
    order.push(await currentPage(page));
    if (i < of) {
      await nav.getByRole('button', { name: 'Next page' }).click();
      await expect(counter(page)).toHaveText(`Page ${i + 1} of ${of}`, { timeout: TURN });
    }
  }
  return order;
}
