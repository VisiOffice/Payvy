// Building a vendor's workflow on its page, and the bills that follow it.
// Only vendor workflows are changed here: the default is shared with other tests.
import { expect, test, type Page } from '@playwright/test';
import { agent, bill, vendor } from './api';

const toast = (page: Page, text: string | RegExp) =>
  page.getByRole('region', { name: 'Notifications' }).getByText(text);

/** A step in the chart being edited (not the "+" before it, whose label names it too). */
const node = (page: Page, text: string | RegExp) =>
  page.getByRole('list', { name: 'Workflow being edited' }).locator('.payvy-wf-node').filter({ hasText: text });

test('a person builds a workflow and new bills follow it', async ({ page }) => {
  const vendorId = await vendor('Canyon Roofing');

  await page.goto('/vendors');
  await page.getByRole('link', { name: 'Canyon Roofing' }).click();
  await expect(page).toHaveURL(`/vendors/${vendorId}`);
  await page.getByRole('button', { name: 'Edit workflow', exact: true }).click();
  await expect(page.getByText(/Canyon Roofing follows Payvy's starting workflow/)).toBeVisible();

  // Add an approval for bills of $2,500 or more, by Robert.
  await page.getByRole('button', { name: /Add a step before Payment released by anyone/ }).click();
  await page.getByRole('menuitem', { name: /Approval/ }).click();
  await page.getByRole('combobox', { name: 'Approved by' }).click();
  await page.getByRole('option', { name: 'Robert' }).click();
  await page.keyboard.press('Escape');
  await page.getByLabel('Only for bills of at least').fill('2,500');
  await expect(node(page, /Approved by Robert for bills of \$2,500\.00 or more/)).toBeVisible();

  // Then wait for the customer's payment; the signal follows the label.
  await page.getByRole('button', { name: /Add a step before Payment released/ }).click();
  await page.getByRole('menuitem', { name: /Wait for/ }).click();
  await expect(node(page, /Wait until something happens/)).toContainText('Say what to wait for');
  await expect(page.getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
  await page.getByRole('textbox', { name: 'Wait until' }).fill("the customer's payment");
  await expect(page.getByRole('textbox', { name: 'Signal' })).toHaveValue('customer_payment');

  await page.getByLabel('Why the change').fill('Roof jobs are paid after the customer pays');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(toast(page, /New bills from Canyon Roofing follow this workflow/)).toBeVisible();
  await expect(page.getByText(/New bills from Canyon Roofing follow this, accepted by robert/)).toBeVisible();

  // The server has it, in order.
  const got = await agent<{ source: string; workflow: { steps: { type: string; signal?: string }[] } }>(
    'workflow.get',
    { vendor_id: vendorId },
  );
  expect(got.source).toBe('vendor');
  expect(got.workflow.steps.map((s) => s.type)).toEqual(['approve', 'wait_for', 'pay']);
  expect(got.workflow.steps[1]?.signal).toBe('customer_payment');

  // A small bill skips the approval and waits for the customer.
  const small = await bill({ vendor_id: vendorId, invoice_number: 'R-1', invoice_date: '2027-03-01', amount_cents: 90_000 });
  const smallBill = await agent<{ progress: { status: string } }>('bill.get', { bill_id: small });
  expect(smallBill.progress.status).toBe('waiting_for');
  // A big one waits for Robert.
  const big = await bill({ vendor_id: vendorId, invoice_number: 'R-2', invoice_date: '2027-03-01', amount_cents: 480_000 });
  const bigBill = await agent<{ progress: { status: string } }>('bill.get', { bill_id: big });
  expect(bigBill.progress.status).toBe('awaiting_approval');

  // Both show on its page, and it's listed as having its own workflow.
  await page.reload();
  await expect(page.getByRole('link', { name: 'R-2' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'R-1' })).toBeVisible();
  await page.goto('/workflows');
  await expect(page.getByRole('row', { name: /Canyon Roofing/ }).getByText('Its own')).toBeVisible();
  await page.getByRole('link', { name: 'Canyon Roofing' }).click();
  await expect(page).toHaveURL(`/vendors/${vendorId}#workflow`);
});

test('an agent’s proposal is reviewed in the builder', async ({ page }) => {
  const vendorId = await vendor('Saguaro Landscaping');
  await agent('workflow.propose', {
    vendor_id: vendorId,
    reason: 'Hold landscaping bills a week',
    workflow: {
      steps: [
        { id: 'hold', type: 'hold', days: 7 },
        { id: 'pay', type: 'pay', release: 'manual' },
      ],
    },
  });

  await page.goto('/vendors');
  await expect(page.getByRole('row', { name: /Saguaro Landscaping/ }).getByText('1 proposed change')).toBeVisible();
  // The old builder address still finds it.
  await page.goto(`/workflows/${vendorId}`);
  await expect(page).toHaveURL(`/vendors/${vendorId}#workflow`);
  await expect(page.getByText(/Proposed by claude-ap on .*: Hold landscaping bills a week/)).toBeVisible();

  // Start from it, change the hold, and save that instead.
  await page.getByRole('button', { name: 'Edit from this' }).click();
  await node(page, /Wait 7 days/).click();
  await page.getByLabel('Days to wait').fill('10');
  await page.getByLabel('Days to wait').press('Enter');
  await expect(node(page, /Wait 10 days/)).toBeVisible();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(toast(page, /New bills from Saguaro Landscaping follow this workflow/)).toBeVisible();

  // The agent's proposal is still open; declining it leaves the saved one.
  await page.getByRole('button', { name: 'Decline' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Decline' }).click();
  await expect(page.getByText('Hold landscaping bills a week')).toHaveCount(0);
  const got = await agent<{ workflow: { steps: { days?: number }[] } }>('workflow.get', { vendor_id: vendorId });
  expect(got.workflow.steps[0]?.days).toBe(10);
});
