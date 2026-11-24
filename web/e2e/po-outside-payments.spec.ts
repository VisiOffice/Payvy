// Paid outside Payvy declarations: an agent key declares an outside payment on
// a PO, a person confirms it in the browser, releases the bill, and sees it
// paid with no bank transfer sent.
import { expect, test, type Page } from '@playwright/test';
import { agent, bill, vendor } from './api';

async function personCommand<T = unknown>(page: Page, command: string, input: object): Promise<T> {
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' },
    data: { mode: 'commit', input },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const body = (await res.json()) as { result: T };
  return body.result;
}

async function confirmPersonCommand<T = unknown>(page: Page, command: string, input: object): Promise<T> {
  const prev = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' },
    data: { mode: 'preview', input },
  });
  expect(prev.ok(), await prev.text()).toBeTruthy();
  const { confirm_token } = (await prev.json()) as { confirm_token: string };
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' },
    data: { mode: 'commit', input, confirm_token },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const body = (await res.json()) as { result: T };
  return body.result;
}

test('agent declares outside payment, person confirms and releases bill with no transfer sent', async ({ page }) => {
  await personCommand(page, 'po.configure', { enabled: true });
  const vendorId = await vendor('Outside Paid Vendor');
  const proposed = await agent<{ proposal_id: string }>('workflow.propose', {
    vendor_id: vendorId,
    workflow: { steps: [{ id: 'pay', type: 'pay', release: 'manual' }] },
    reason: 'Manual release for outside-payment test',
  });
  await personCommand(page, 'workflow.accept', { proposal_id: proposed.proposal_id });

  // Create an agent key, authorize it as the PO source, upsert an active PO, and declare outside payment.
  const key = await personCommand<{ id: string; key: string }>(page, 'agent_key.create', {
    name: 'orders-sync',
  });
  const meRes = await page.request.get('/auth/session', { headers: { 'x-payvy-client': 'web' } });
  expect(meRes.ok(), await meRes.text()).toBeTruthy();
  const ownerActor = ((await meRes.json()) as { person: { actor: string } }).person.actor;

  await personCommand(page, 'po.source_authorize', {
    source: 'orders',
    key_id: key.id,
    vendor_ids: [vendorId],
    max_cents: 500_000,
    owner: ownerActor,
  });

  async function sourceCommand(command: string, input: object) {
    const res = await page.request.post(`/api/commands/${command}`, {
      headers: { authorization: `Bearer ${key.key}` },
      data: { mode: 'commit', input },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    return (await res.json()) as { result: Record<string, unknown> };
  }

  const po = await sourceCommand('po.upsert', {
    vendor_id: vendorId,
    po_number: 'PO-OUT-1',
    external_source: 'orders',
    external_id: 'ext-out-1',
    authorized_cents: 100_000,
    lines: [{ description: 'Field service', amount_cents: 100_000 }],
  });
  const poId = po.result.id as string;

  const today = new Date().toISOString().slice(0, 10);
  const billId = await bill({
    vendor_id: vendorId,
    invoice_number: 'INV-OUT-1',
    invoice_date: today,
    amount_cents: 75_000,
  });
  await confirmPersonCommand(page, 'po.match_apply', {
    bill_id: billId,
    allocations: [{ po_id: poId, amount_cents: 75_000 }],
  });

  await sourceCommand('po.declare_paid', {
    external_source: 'orders',
    external_id: 'ext-out-1',
    amount_cents: 75_000,
    method: 'card',
    paid_on: today,
    reference: 'AUTH-7781',
    note: 'Charged by phone on site',
  });

  // Person opens Purchase orders, selects PO-OUT-1, and confirms the declaration.
  await page.goto('/purchase-orders');
  await page.getByRole('button', { name: 'PO-OUT-1' }).click();
  await expect(page.getByText('Paid outside Payvy', { exact: true })).toBeVisible();
  await expect(page.getByText('Charged by phone on site')).toBeVisible();
  await page.getByRole('button', { name: 'Confirm', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Confirm', exact: true })).toHaveCount(0);

  // Person opens the bill, sees the paid-outside banner, releases it, and sees it paid with no transfer.
  await page.goto(`/bills/${billId}`);
  await expect(
    page.getByText(/Paid outside Payvy per declaration on PO-OUT-1 — waiting for release/),
  ).toBeVisible();
  await page.getByRole('button', { name: 'Pay…' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText(/Releasing sends no payment/)).toBeVisible();
  await dialog.getByRole('button', { name: 'Release', exact: true }).click();
  await expect(dialog).toHaveCount(0);

  await expect(
    page.getByText(/Paid outside Payvy per declaration on PO-OUT-1 — paid/),
  ).toBeVisible();

  const finalBill = await agent<{ bill: { state: string; paid_reference: string }; payments: unknown[] }>(
    'bill.get',
    { bill_id: billId },
  );
  expect(finalBill.bill.state).toBe('paid');
  expect(finalBill.bill.paid_reference).toBe('AUTH-7781');
  expect(finalBill.payments).toHaveLength(0);
});
