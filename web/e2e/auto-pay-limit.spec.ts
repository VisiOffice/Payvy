// The auto-pay limit: a bill over it waits for a person even though its
// workflow pays automatically, and says why. Raising the limit is previewed
// and confirmed; a vendor's own limit is set on its page. No bank is
// connected (the bill tests expect none), so nothing is sent; where a bill
// stands is all this checks. The company limit ends where it started.
import { expect, test, type Page } from '@playwright/test';
import { agent, bill, vendor } from './api';

async function personCommand(page: Page, command: string, input: object) {
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' },
    data: { mode: 'commit', input },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()).result;
}

test('a bill over the auto-pay limit waits for a person until the limit allows it', async ({ page }) => {
  const id = await vendor('Ceiling Supply');
  const proposed = await agent<{ proposal_id: string }>('workflow.propose', {
    vendor_id: id,
    workflow: { steps: [{ id: 'pay', type: 'pay', release: 'auto' }] },
  });
  await personCommand(page, 'workflow.accept', { proposal_id: proposed.proposal_id });
  // Agents can't change the limit.
  await expect(agent('auto_pay.lower_limit', { limit_cents: 100 })).rejects.toThrow(/403|person/);
  const billId = await bill({ vendor_id: id, invoice_number: 'CS-6000', invoice_date: '2027-01-15', amount_cents: 600_000 });

  // Waiting on you shows it to release, with the reason.
  await page.goto('/');
  const row = page.locator('.payvy-queue-row').filter({ hasText: 'Ceiling Supply' });
  await expect(row.getByText('Over the $5,000.00 auto-pay limit')).toBeVisible();
  await expect(row.getByRole('button', { name: 'Pay…' })).toBeVisible();
  // So does the bill's page.
  await page.goto(`/bills/${billId}`);
  await expect(page.getByText('Over the $5,000.00 auto-pay limit')).toBeVisible();
  await expect(page.getByText('Ready to pay').first()).toBeVisible();

  // Raising the company limit shows which bills would start paying, then confirms.
  await page.goto('/settings/auto-pay');
  await expect(page.getByRole('heading', { name: 'Auto-pay limit', level: 1 })).toBeVisible();
  await expect(page.getByText('(the default; no one has set it yet)')).toBeVisible();
  await page.getByLabel('Company limit').fill('7,500');
  await page.getByRole('button', { name: 'Save limit' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText("Raise the company's auto-pay limit?")).toBeVisible();
  await expect(dialog.getByText('Ceiling Supply CS-6000')).toBeVisible();
  await dialog.getByRole('button', { name: 'Raise limit' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByText('$7,500.00', { exact: true })).toBeVisible();

  await page.goto(`/bills/${billId}`);
  await expect(page.getByText('Pays automatically').first()).toBeVisible();
  await expect(page.getByText(/auto-pay limit/)).toHaveCount(0);

  // A vendor's own, lower limit holds it again; removing it is confirmed too.
  await page.goto(`/vendors/${id}`);
  const section = page.locator('#auto-pay');
  await section.getByLabel('Limit for this vendor').fill('1000');
  await section.getByRole('button', { name: 'Save limit' }).click();
  await expect(section.getByText('This vendor has its own limit')).toBeVisible();
  await page.goto(`/bills/${billId}`);
  await expect(page.getByText("Over this vendor's $1,000.00 auto-pay limit")).toBeVisible();
  await page.goto(`/vendors/${id}`);
  await section.getByRole('button', { name: 'Use company limit' }).click();
  await expect(page.getByRole('dialog').getByText('These bills will start paying automatically:')).toBeVisible();
  await page.getByRole('dialog').getByRole('button', { name: 'Raise limit' }).click();
  await expect(section.getByText("That's the company's limit")).toBeVisible();

  // Lowering is immediate. Back to $5,000.00, and the bill waits again.
  await page.goto('/settings/auto-pay');
  await page.getByLabel('Company limit').fill('5000');
  await page.getByRole('button', { name: 'Save limit' }).click();
  await expect(page.getByText('$5,000.00', { exact: true })).toBeVisible();
  await page.goto(`/bills/${billId}`);
  await expect(page.getByText('Over the $5,000.00 auto-pay limit')).toBeVisible();

  // Every change is in the log, made by the person.
  const log = await personCommand(page, 'event.list', { limit: 50 });
  const kinds = (log.events as { kind: string; actor: string }[]).filter((e) => e.kind.startsWith('auto_pay.'));
  expect(kinds.map((e) => e.kind)).toEqual([
    'auto_pay.limit_lowered',
    'auto_pay.limit_raised',
    'auto_pay.limit_lowered',
    'auto_pay.limit_raised',
  ]);
  expect(kinds.every((e) => e.actor === 'robert')).toBeTruthy();

  // Leave nothing behind for other tests.
  await personCommand(page, 'bill.reject', { bill_id: billId, reason: 'test bill' });
});
