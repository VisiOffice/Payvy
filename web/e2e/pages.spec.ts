// Arranging a document's pages on document review: reorder, rotate, hide and
// restore, each saved at once as a new version, the history in words, the
// bill showing the arrangement it pinned, and the lock after approval.
import { expect, test, type Page } from '@playwright/test';
import { agent, vendor } from './api';
import { pdfPages } from './pdf';

/** A three-page PDF no other test has. */
async function threePages(tag: string): Promise<string> {
  const bytes = pdfPages([
    ['Invoice', `Acme ${tag}`, 'Total $90.00'],
    ['Terms and conditions', tag],
    ['Remittance slip', tag],
  ]);
  const doc = await agent<{ id: string }>('document.upload', {
    filename: `acme-${tag}.pdf`,
    content_base64: bytes.toString('base64'),
  });
  return doc.id;
}

/** What only a person may do, as the signed-in person. */
async function personCommand(page: Page, command: string, input: object) {
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' },
    data: { mode: 'commit', input },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()).result;
}

/** A vendor whose bills need one approval. */
async function approvedVendor(page: Page, name: string): Promise<string> {
  const id = await vendor(name);
  const proposed = await agent<{ proposal_id: string }>('workflow.propose', {
    vendor_id: id,
    workflow: {
      steps: [
        { id: 'ok', type: 'approve' },
        { id: 'pay', type: 'pay', release: 'manual' },
      ],
    },
  });
  await personCommand(page, 'workflow.accept', { proposal_id: proposed.proposal_id });
  return id;
}

/** The original's page numbers in the order the viewer shows them. */
const shown = (page: Page) =>
  page.locator('.payvy-viewer-canvas [data-page]').evaluateAll((els) => els.map((e) => Number(e.getAttribute('data-page'))));

const strip = (page: Page) => page.getByRole('region', { name: 'Pages' });
const history = (page: Page) => page.getByRole('list', { name: 'Page history' });

test('pages are reordered, rotated, hidden and restored, then lock once the bill is approved', async ({ page }) => {
  const doc = await threePages('desk');
  await page.goto(`/documents/${doc}`);
  await expect(strip(page).getByText('3 pages')).toBeVisible();
  await expect(strip(page).getByRole('img', { name: 'Page 3' })).toBeVisible();
  await expect.poll(() => shown(page)).toEqual([1, 2, 3]);

  // Move page 3 to the front, a step at a time.
  await strip(page).getByRole('button', { name: 'Move page 3 earlier' }).click();
  await expect(history(page).getByText('Robert moved page 3 to position 2')).toBeVisible();
  await strip(page).getByRole('button', { name: 'Move page 3 earlier' }).click();
  await expect(history(page).getByText('Robert moved page 3 to first')).toBeVisible();
  await expect.poll(() => shown(page)).toEqual([3, 1, 2]);
  // At the front it can't go earlier.
  await expect(strip(page).getByRole('button', { name: 'Move page 3 earlier' })).toBeDisabled();

  // From the keyboard: the focus stays on the control just used.
  const rotate = strip(page).getByRole('button', { name: 'Rotate page 1' });
  await rotate.focus();
  await page.keyboard.press('Enter');
  await expect(history(page).getByText('Robert rotated page 1 to 90°')).toBeVisible();
  await expect(rotate).toBeFocused();
  await expect(page.locator('.payvy-viewer-canvas [data-page="1"]')).toHaveAttribute('data-rotation', '90');

  // Hidden pages leave the view and show greyed with Restore.
  await strip(page).getByRole('button', { name: 'Hide page 2' }).click();
  await expect(history(page).getByText('Robert hid page 2')).toBeVisible();
  await expect.poll(() => shown(page)).toEqual([3, 1]);
  await expect(page.getByText('2 pages · 1 hidden')).toBeVisible();
  await expect(strip(page).getByRole('listitem', { name: 'Page 2, hidden' })).toBeVisible();
  await strip(page).getByRole('button', { name: 'Restore page 2' }).click();
  await expect(history(page).getByText('Robert restored page 2')).toBeVisible();
  await expect.poll(() => shown(page)).toEqual([3, 1, 2]);

  // An agent's change is recorded under its name and shows on reload.
  const before = await agent<{ pages: { version: number; arrangement: object[] } }>('document.get', { document_id: doc });
  expect(before.pages.version).toBe(5);
  await agent('document.arrange', {
    document_id: doc,
    expected_version: 5,
    pages: [{ page: 3 }, { page: 1, rotation: 90 }, { page: 2, hidden: true }],
  });
  await page.reload();
  await expect(history(page).getByText('claude-ap hid page 2')).toBeVisible();
  await expect.poll(() => shown(page)).toEqual([3, 1]);

  // A bill made from it shows the arrangement it pinned.
  const v = await approvedVendor(page, 'Acme Pages Desk');
  const bill = await agent<{ bill: { id: string } }>('bill.create', {
    vendor_id: v,
    invoice_number: 'AP-DESK',
    invoice_date: '2026-11-01',
    amount_cents: 9000,
    document_id: doc,
  });
  // Before approval a change moves the bill along, on its history.
  await page.reload();
  await strip(page).getByRole('button', { name: 'Restore page 2' }).click();
  await expect(history(page).getByText('Robert restored page 2')).toHaveCount(2);
  await page.goto(`/bills/${bill.bill.id}`);
  await expect(page.getByText('Pages rearranged: restored page 2')).toBeVisible();
  await expect.poll(() => shown(page)).toEqual([3, 1, 2]);

  // Approved: the pages lock, saying why, and the controls are off.
  await personCommand(page, 'bill.approve', { bill_id: bill.bill.id });
  await page.goto(`/documents/${doc}`);
  await expect(strip(page).getByText('Pages are locked after approval')).toBeVisible();
  for (const name of ['Move page 1 earlier', 'Rotate page 2', 'Hide page 3']) {
    await expect(strip(page).getByRole('button', { name })).toBeDisabled();
  }
  await expect(
    agent('document.arrange', { document_id: doc, expected_version: 7, pages: [{ page: 1 }, { page: 2 }, { page: 3 }] }),
  ).rejects.toThrow(/Pages are locked after approval/);
});

test('pages can be arranged on a phone, inside the original document tab', async ({ page }, info) => {
  const doc = await threePages('phone');
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/documents/${doc}`);
  await page.getByRole('radio', { name: 'Original document', exact: true }).click();
  await expect(strip(page).getByRole('img', { name: 'Page 2' })).toBeVisible();
  await expect(page.locator('.payvy-viewer-canvas canvas').first()).toBeVisible();

  await strip(page).getByRole('button', { name: 'Move page 2 earlier' }).click();
  await expect.poll(() => shown(page)).toEqual([2, 1, 3]);
  await strip(page).getByRole('button', { name: 'Rotate page 3' }).click();
  await expect(page.locator('.payvy-viewer-canvas [data-page="3"]')).toHaveAttribute('data-rotation', '90');
  await strip(page).getByRole('button', { name: 'Hide page 1' }).click();
  await expect.poll(() => shown(page)).toEqual([2, 3]);
  await strip(page).getByRole('button', { name: 'Restore page 1' }).click();
  await expect.poll(() => shown(page)).toEqual([2, 1, 3]);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath('pages-mobile-original.png'), animations: 'disabled' });

  // The history is with the invoice details.
  await page.getByRole('radio', { name: 'Invoice details', exact: true }).click();
  await expect(history(page).getByText('Robert restored page 1')).toBeVisible();
  await expect(history(page).getByText('Robert hid page 1')).toBeVisible();
  await expect(history(page).getByText('Robert rotated page 3 to 90°')).toBeVisible();
  await expect(history(page).getByText('Robert moved page 2 to first')).toBeVisible();

  // Locked after approval, on the phone too.
  const v = await approvedVendor(page, 'Acme Pages Phone');
  const bill = await agent<{ bill: { id: string } }>('bill.create', {
    vendor_id: v,
    invoice_number: 'AP-PHONE',
    invoice_date: '2026-11-01',
    amount_cents: 9000,
    document_id: doc,
  });
  await personCommand(page, 'bill.approve', { bill_id: bill.bill.id });
  await page.reload();
  await page.getByRole('radio', { name: 'Original document', exact: true }).click();
  await expect(strip(page).getByText('Pages are locked after approval')).toBeVisible();
  await expect(strip(page).getByRole('button', { name: 'Rotate page 1' })).toBeDisabled();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: info.outputPath('pages-mobile-locked.png'), animations: 'disabled' });

  // The bill page shows the pinned arrangement at this width as well.
  await page.goto(`/bills/${bill.bill.id}`);
  await expect.poll(() => shown(page)).toEqual([2, 1, 3]);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});
