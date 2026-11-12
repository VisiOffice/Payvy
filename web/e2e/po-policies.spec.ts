// The enumerated policy registry and the suspended lifecycle, through the
// app: an admin suspends and reactivates a PO, and sets a vendor's
// over-period policy. Setup data is made as an agent would.
import { expect, test, type Page } from '@playwright/test';
import { agent, vendor } from './api';

async function command(page: Page, name: string, input: object) {
  const r = await page.request.post(`/api/commands/${name}`, { headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input } });
  expect(r.ok(), await r.text()).toBe(true);
  return (await r.json()).result;
}

test('a person suspends and reactivates a PO, and sets the over-period policy', async ({ page }) => {
  await page.goto('/settings/purchase-orders');
  const toggle = page.getByRole('switch', { name: 'Use purchase orders', exact: true });
  if (!(await toggle.isChecked())) await toggle.click();
  await expect(toggle).toBeChecked();

  const v = await vendor('Policy Hauler');
  await agent('po.create', { vendor_id: v, po_number: 'PO-POL-1', owner: 'robert', authorized_cents: 100_000 });
  const list = await agent<{ purchase_orders: { id: string; po_number: string }[] }>('po.list', {});
  const po = list.purchase_orders.find((p) => p.po_number === 'PO-POL-1');
  expect(po).toBeTruthy();
  await command(page, 'po.activate', { po_id: po!.id });

  await page.goto('/purchase-orders');
  await page.getByRole('button', { name: 'PO-POL-1', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Suspend…' })).toBeVisible();

  // Suspend with a reason: the way back is offered, and the badge says so.
  await page.getByRole('button', { name: 'Suspend…' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Why').fill('Order suspended by the customer');
  await dialog.getByRole('button', { name: 'Suspend' }).click();
  await expect(page.getByRole('button', { name: 'Reactivate' })).toBeVisible();
  await expect(page.getByText('Suspended', { exact: true }).last()).toBeVisible();

  await page.getByRole('button', { name: 'Reactivate' }).click();
  await expect(page.getByRole('button', { name: 'Suspend…' })).toBeVisible();

  // The vendor's matching policy, on its own page.
  await page.goto(`/vendors/${v}`);
  const policySwitch = page.getByRole('switch', { name: "An invoice outside a PO's period can't be covered by it" });
  await policySwitch.click();
  await expect(page.getByRole('region', { name: 'Notifications' }).getByText('Matching policy saved')).toBeVisible();
  await expect(policySwitch).toBeChecked();
  await expect(page.getByText(/Policy version 1/)).toBeVisible();

  // Leave the company as the suite expects to find it: purchase orders off.
  await page.goto('/settings/purchase-orders');
  if (await toggle.isChecked()) await toggle.click();
  await expect(toggle).not.toBeChecked();
});

test('quantities show ordered, billed and remaining on the bill', async ({ page }) => {
  await page.goto('/settings/purchase-orders');
  const toggle = page.getByRole('switch', { name: 'Use purchase orders', exact: true });
  if (!(await toggle.isChecked())) await toggle.click();
  await expect(toggle).toBeChecked();
  try {
    const v = await vendor('Quantity Hauler');
    const created = await agent<{ id: string }>('po.create', {
      vendor_id: v, po_number: 'PO-QTY-1', owner: 'robert', authorized_cents: 100_000,
      lines: [{ description: 'Haul', quantity: 3, unit: 'load', unit_price_cents: 10_000, amount_cents: 30_000 }],
    });
    await command(page, 'po.set_vendor_settings', { vendor_id: v, require_po: true });
    await command(page, 'po.activate', { po_id: created.id });
    const b = await agent<{ bill: { id: string } }>('bill.create', {
      vendor_id: v, invoice_number: 'QTY-1', invoice_date: '2027-01-15', amount_cents: 20_000,
      lines: [{ description: 'Haul', amount_cents: 20_000, quantity: '2', unit: 'load', unit_price_cents: 10_000 }],
    });
    // Reserve through the confirmed preview, as the app does.
    const preview = await page.request.post('/api/commands/po.match_apply', {
      headers: { 'x-payvy-client': 'web' },
      data: { mode: 'preview', input: { bill_id: b.bill.id, allocations: [{ po_id: created.id, amount_cents: 20_000 }] } },
    });
    const token = (await preview.json()).confirm_token as string;
    const applied = await page.request.post('/api/commands/po.match_apply', {
      headers: { 'x-payvy-client': 'web' },
      data: { mode: 'commit', confirm_token: token, input: { bill_id: b.bill.id, allocations: [{ po_id: created.id, amount_cents: 20_000 }] } },
    });
    expect(applied.ok(), await applied.text()).toBe(true);

    await page.goto(`/bills/${b.bill.id}`);
    await expect(page.getByText('Quantities')).toBeVisible();
    await expect(page.getByText(/2 of 3 load billed · 1 left/)).toBeVisible();

    // Under an enforcing policy the finding itself renders on the bill.
    await command(page, 'po.policy_set', { vendor_id: v, quantity_matching: 'enforce' });
    const over = await agent<{ bill: { id: string } }>('bill.create', {
      vendor_id: v, invoice_number: 'QTY-2', invoice_date: '2027-01-15', amount_cents: 20_000,
      lines: [{ description: 'Haul', amount_cents: 20_000, quantity: '2', unit: 'load', unit_price_cents: 10_000 }],
    });
    const preview2 = await page.request.post('/api/commands/po.match_apply', {
      headers: { 'x-payvy-client': 'web' },
      data: { mode: 'preview', input: { bill_id: over.bill.id, allocations: [{ po_id: created.id, amount_cents: 20_000 }] } },
    });
    const token2 = (await preview2.json()).confirm_token as string;
    await page.request.post('/api/commands/po.match_apply', {
      headers: { 'x-payvy-client': 'web' },
      data: { mode: 'commit', confirm_token: token2, input: { bill_id: over.bill.id, allocations: [{ po_id: created.id, amount_cents: 20_000 }] } },
    });
    await page.goto(`/bills/${over.bill.id}`);
    // The blocking finding shows once, as the payment gate's banner.
    await expect(page.getByText(/Can't be paid yet: quantity over/)).toBeVisible();
    await expect(page.getByText(/4 of 3 load billed · 0 left/)).toBeVisible();
  } finally {
    await page.goto('/settings/purchase-orders');
    if (await toggle.isChecked()) await toggle.click();
    await expect(toggle).not.toBeChecked();
  }
});
