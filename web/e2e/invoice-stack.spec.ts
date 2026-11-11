// A vendor sending the same invoice twice stacks on one bill, never a
// second payable one; a revised amount waits for a person on the bill page.
import { expect, test } from '@playwright/test';
import { agent, bill, vendor } from './api';

test('a resent invoice stacks, a revision is flagged, and a person accepts it', async ({ page }) => {
  const v = await vendor('Stacked Hauler');
  const id = await bill({ vendor_id: v, invoice_number: 'R260405861', invoice_date: '2026-11-10', amount_cents: 8_740 });
  // The same invoice at the same amount, then a revised copy at $37.49.
  const resent = await agent<{ bill: { id: string }; received_count: number }>('bill.create', {
    vendor_id: v, invoice_number: 'R260405861', invoice_date: '2026-11-10', amount_cents: 8_740,
  });
  expect(resent.bill.id).toBe(id);
  expect(resent.received_count).toBe(2);
  await agent('bill.create', { vendor_id: v, invoice_number: 'R260405861', invoice_date: '2026-11-10', amount_cents: 3_749 });

  // The list marks what arrived, at a glance.
  await page.goto('/bills');
  // Other tests stack copies too, so look at this bill's row.
  const row = page.getByRole('row').filter({ hasText: 'R260405861' });
  await expect(row.getByText('Sent 3×')).toBeVisible();
  await expect(row.getByText('Revised', { exact: true })).toBeVisible();

  // The bill shows both amounts and waits for a person's decision.
  await page.goto(`/bills/${id}`);
  await expect(page.getByText('Sent 3×')).toBeVisible();
  await expect(page.getByText('Revised invoice received')).toBeVisible();
  await expect(page.getByText(/carries \$87\.40.*carries \$37\.49/)).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Received 3 times' })).toBeVisible();

  // Accepting makes the latest amount the payable one.
  await page.getByRole('button', { name: 'Accept revision' }).click();
  await expect(
    page.getByRole('region', { name: 'Notifications' }).getByText(/Revision accepted: Stacked Hauler R260405861/),
  ).toBeVisible();
  await expect(page.getByText('Revised invoice received')).toHaveCount(0);
  await expect(page.getByText('$37.49', { exact: true }).first()).toBeVisible();
  await expect(page.getByText(/Accepted by robert/)).toBeVisible();
});
