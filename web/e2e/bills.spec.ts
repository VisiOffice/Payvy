// The paths that end in money moving, or in deciding it shouldn't: an
// emailed invoice becoming a bill, approval, release with the server's
// preview and one-use confirmation, rejection, and setting documents aside.
import { expect, test, type Page } from '@playwright/test';
import { agent, bill, inboxDocument, vendor } from './api';

/** The innermost element holding both the text and the button: one card. */
function card(page: Page, text: string, button: string) {
  return page
    .locator('div')
    .filter({ has: page.getByText(text, { exact: true }) })
    .filter({ has: page.getByRole('button', { name: button }) })
    .last();
}

/** A toast; its text is also read out by a live region, so look in the toasts. */
const toast = (page: Page, text: string) =>
  page.getByRole('region', { name: 'Notifications' }).getByText(text);

test('an invoice is checked, becomes a bill, and is released', async ({ page }) => {
  await vendor('Desert Plumbing & Drain');
  const doc = await inboxDocument('desert-plumbing-5521.pdf');

  await page.goto('/documents');
  await page.getByRole('link', { name: 'desert-plumbing-5521.pdf' }).click();
  await expect(page).toHaveURL(`/documents/${doc}`);

  // Filled in from what the model read, and found on the page.
  await expect(page.getByLabel('Invoice number')).toHaveValue('DP-5521');
  await expect(page.getByLabel('Amount')).toHaveValue('1420.00');
  await expect(page.getByText(/Matched Desert Plumbing & Drain/)).toBeVisible();
  await expect(page.locator('.payvy-hl[data-hl="invoice_number"]')).toBeVisible();
  await expect(page.locator('.payvy-hl[data-hl="total"]')).toBeVisible();
  await expect(page.getByText('Not found on the page')).toHaveCount(0);
  await page.getByLabel('Amount').focus();
  await expect(page.locator('.payvy-hl-focus[data-hl="total"]')).toBeVisible();

  await page.getByRole('button', { name: 'Create bill' }).click();
  await expect(page).toHaveURL(/\/bills\/[0-9a-f-]{36}$/);
  await expect(page.getByRole('heading', { name: 'Desert Plumbing & Drain' })).toBeVisible();
  await expect(page.getByText('Ready to pay')).toBeVisible();
  await expect(page.getByText('Replace 40 gal water heater')).toBeVisible();

  // No bank is connected: the preview says so, and the bill is paid outside.
  await page.getByRole('button', { name: 'Pay…' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('Release $1,420.00 to Desert Plumbing & Drain')).toBeVisible();
  await dialog.getByRole('button', { name: /^Release/ }).click();
  await expect(toast(page, "Released. Mark it paid once you've paid it.")).toBeVisible();
  await expect(page.getByText('Released', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Pay…' })).toHaveCount(0);

  // The document is filed with the bill.
  await page.goto(`/documents/${doc}`);
  await expect(page.getByText('This document is a bill')).toBeVisible();
});

test('a workflow an agent proposes needs a person, then bills wait for approval', async ({ page }) => {
  const mesa = await vendor('Mesa Electric Supply Co.');
  await agent('workflow.propose', {
    vendor_id: mesa,
    reason: 'Electrical supplies over $100 need Robert',
    workflow: {
      steps: [
        { id: 'robert', type: 'approve', approvers: ['robert'], min_amount_cents: 10_000 },
        { id: 'pay', type: 'pay', release: 'manual' },
      ],
    },
  });

  await page.goto('/');
  await expect(page.getByText('Electrical supplies over $100 need Robert')).toBeVisible();
  await card(page, 'Mesa Electric Supply Co.', 'Accept').getByRole('button', { name: 'Accept' }).click();
  await expect(page.getByText('Electrical supplies over $100 need Robert')).toHaveCount(0);

  // Read from the document; an agent enters it as suggested.
  const doc = await inboxDocument('mesa-electric-30117.pdf');
  const read = await agent<{ extractions: { bill_input: object | null }[] }>('document.get', { document_id: doc });
  const input = read.extractions[0]?.bill_input;
  expect(input).toBeTruthy();
  await agent('bill.create', input!);

  await page.reload();
  const waiting = card(page, 'Mesa Electric Supply Co.', 'Approve');
  await expect(waiting.getByText('$341.63')).toBeVisible();
  await waiting.getByRole('button', { name: 'Approve' }).click();
  await expect(toast(page, 'Approved Mesa Electric Supply Co. 30117')).toBeVisible();

  // Approved: now it waits to be paid.
  await expect(card(page, 'Mesa Electric Supply Co.', 'Pay…')).toBeVisible();
});

test('agents can neither approve nor release', async () => {
  const v = await vendor('Valley Fuel');
  const id = await bill({ vendor_id: v, invoice_number: 'VF-1', invoice_date: '2026-11-01', amount_cents: 5000 });
  // Approving and releasing are a person's acts, whatever the workflow.
  await expect(agent('bill.approve', { bill_id: id })).rejects.toThrow(/403|person/);
  await expect(agent('bill.release', { bill_id: id, pay_elsewhere: true })).rejects.toThrow(/403|person/);
});

test('a bill is rejected with a reason', async ({ page }) => {
  const v = await vendor('Copper State Roofing');
  const id = await bill({ vendor_id: v, invoice_number: 'CS-77', invoice_date: '2026-11-03', amount_cents: 98_000 });

  await page.goto(`/bills/${id}`);
  await page.getByRole('button', { name: 'Reject' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Why').fill('Roof work was never done at this property');
  await dialog.getByRole('button', { name: 'Reject' }).click();
  await expect(page.getByText('Rejected', { exact: true }).first()).toBeVisible();
  await expect(page.getByRole('button', { name: 'Pay…' })).toHaveCount(0);
});

test('a likely duplicate is caught, and entered only on purpose', async ({ page }) => {
  const king = await vendor('King Environmental, LLC');
  await bill({ vendor_id: king, invoice_number: '189727', invoice_date: '2026-11-01', amount_cents: 45_900 });
  const doc = await inboxDocument('king-environmental-189727.pdf');

  await page.goto(`/documents/${doc}`);
  await expect(page.getByText('Check these before making it a bill')).toBeVisible();
  await page.getByRole('button', { name: 'Create bill' }).click();
  await expect(page.getByText('This looks like a bill you already have')).toBeVisible();
  await expect(page).toHaveURL(`/documents/${doc}`);

  await page.getByRole('button', { name: "It's a separate bill" }).click();
  await expect(page).toHaveURL(/\/bills\/[0-9a-f-]{36}$/);
});

test('a statement is set aside as not a bill', async ({ page }) => {
  const doc = await inboxDocument('acme-supply-statement.pdf');
  await page.goto(`/documents/${doc}`);
  await page.getByRole('button', { name: 'Not a bill' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Why').fill('Monthly statement, not an invoice');
  await dialog.getByRole('button', { name: 'Set aside' }).click();
  await expect(page).toHaveURL('/documents');
  await expect(page.getByRole('link', { name: 'acme-supply-statement.pdf' })).toHaveCount(0);

  await page.getByRole('radio', { name: 'Not bills' }).click();
  await expect(page.getByRole('link', { name: 'acme-supply-statement.pdf' })).toBeVisible();
  await expect(page.getByText('Monthly statement, not an invoice')).toBeVisible();
});
