// The paths that end in money moving, or in deciding it shouldn't: an
// emailed invoice becoming a bill, approval, release with the server's
// preview and one-use confirmation, rejection, and setting documents aside.
import { expect, test, type Page } from '@playwright/test';
import { agent, bill, inboxDocument, vendor } from './api';
import { pdf } from './pdf';

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

  await page.goto('/inbox');
  await page.getByRole('link', { name: 'desert-plumbing-5521.pdf' }).click();
  await expect(page).toHaveURL(`/inbox/${doc}`);

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
  await page.goto(`/inbox/${doc}`);
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

test('a copy the vendor sent again stacks on the bill it belongs to', async ({ page }) => {
  const king = await vendor('King Environmental, LLC');
  const existing = await bill({ vendor_id: king, invoice_number: '189727', invoice_date: '2026-11-01', amount_cents: 45_900 });
  const doc = await inboxDocument('king-environmental-189727.pdf');

  await page.goto(`/inbox/${doc}`);
  // The pre-check says what Create bill will do before it is pressed, and
  // which kind of copy this is at the amount on the form.
  await expect(page.getByText('Create bill will stack this copy on an existing bill')).toBeVisible();
  await expect(page.getByText(/a resend\. It will be recorded on that bill/)).toBeVisible();
  await expect(page.getByRole('button', { name: "It's a separate bill" })).toBeInViewport();

  // Change the amount and the same notice calls it a revision instead.
  await page.getByLabel('Amount').fill('400.00');
  await expect(page.getByText(/That bill carries \$459\.00; this copy carries \$400\.00: a revision/)).toBeVisible();

  await page.getByRole('button', { name: 'Create bill' }).click();
  await expect(page).toHaveURL(`/bills/${existing}`);
  await expect(page.getByText('Revised invoice received')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Received 2 times' })).toBeVisible();
});

test('a copy entered on purpose as a separate bill makes its own bill', async ({ page }) => {
  const saguaro = await vendor('Saguaro Glass & Mirror');
  const existing = await bill({ vendor_id: saguaro, invoice_number: 'SG-4120', invoice_date: '2026-10-30', amount_cents: 51_200 });
  // A file of its own: the server refuses the same file twice.
  const doc = (
    await agent<{ id: string }>('document.upload', {
      filename: 'saguaro-sg-4120.pdf',
      content_base64: pdf(['Saguaro Glass & Mirror', 'Invoice SG-4120', 'Date 2026-11-04', 'Total 512.00']).toString('base64'),
    })
  ).id;
  await agent('document.extract', { document_id: doc });

  await page.goto(`/inbox/${doc}`);
  await expect(page.getByText('Create bill will stack this copy on an existing bill')).toBeVisible();
  await page.getByRole('button', { name: "It's a separate bill" }).click();
  await expect(page).toHaveURL(/\/bills\/[0-9a-f-]{36}$/);
  expect(page.url().endsWith(existing)).toBe(false);
  await expect(page.getByRole('heading', { name: 'Saguaro Glass & Mirror' })).toBeVisible();
});

test('a statement is set aside as not a bill', async ({ page }) => {
  const doc = await inboxDocument('acme-supply-statement.pdf');
  await page.goto(`/inbox/${doc}`);
  await page.getByRole('button', { name: 'Not a bill' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Why').fill('Monthly statement, not an invoice');
  await dialog.getByRole('button', { name: 'Set aside' }).click();
  await expect(page).toHaveURL('/inbox');
  await expect(page.getByRole('link', { name: 'acme-supply-statement.pdf' })).toHaveCount(0);

  await page.getByRole('radio', { name: 'Not bills' }).click();
  await expect(page.getByRole('link', { name: 'acme-supply-statement.pdf' })).toBeVisible();
  await expect(page.getByText('Monthly statement, not an invoice')).toBeVisible();
});
