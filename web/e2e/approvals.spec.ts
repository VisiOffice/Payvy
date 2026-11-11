// Separation of duties: a person turns on "whoever enters a bill can't
// approve it" in Settings, and the app stops offering Approve on their own
// bills, saying who else can. Bills an agent entered are unaffected.
import { expect, test, type Page } from '@playwright/test';
import { agent, bill, vendor } from './api';

/** A command as the signed-in person, the way the app calls it. */
async function asPerson<R = Record<string, unknown>>(page: Page, command: string, input: object): Promise<R> {
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' },
    data: { input, mode: 'commit' },
  });
  const body = await res.json();
  if (!res.ok()) throw new Error(`${command}: ${res.status()} ${JSON.stringify(body)}`);
  return body.result as R;
}

const toast = (page: Page, text: string) =>
  page.getByRole('region', { name: 'Notifications' }).getByText(text);

const RULE = "The person who enters a bill can't approve it";

test('whoever enters a bill waits for someone else to approve it', async ({ page }) => {
  await asPerson(page, 'person.add', { name: 'Maria', email: 'maria@example.com', roles: ['approver'] });
  const v = await vendor('Saguaro Landscaping');
  const proposal = await agent<{ proposal_id: string }>('workflow.propose', {
    vendor_id: v,
    workflow: {
      steps: [
        { id: 'ok', type: 'approve', approvers: ['robert', 'maria'] },
        { id: 'pay', type: 'pay', release: 'manual' },
      ],
    },
  });
  await asPerson(page, 'workflow.accept', { proposal_id: proposal.proposal_id });

  try {
    // Off by default; a person turns it on in Settings > Approvals.
    await page.goto('/');
    await page.getByRole('link', { name: 'Approvals' }).click();
    await expect(page).toHaveURL(/\/settings\/approvals$/);
    const rule = page.getByLabel(RULE);
    await expect(rule).not.toBeChecked();
    await rule.click();
    await expect(toast(page, 'Someone else approves what a person enters')).toBeVisible();
    await expect(rule).toBeChecked();
    expect((await agent<{ submitter_cannot_approve: boolean }>('approval_rules.get', {})).submitter_cannot_approve).toBe(
      true,
    );
    await expect(page.getByText(/ by robert$/)).toBeVisible();

    // Robert enters one bill himself; an agent enters another.
    const own = (
      await asPerson<{ bill: { id: string } }>(page, 'bill.create', {
        vendor_id: v,
        invoice_number: 'SL-100',
        invoice_date: '2026-11-04',
        amount_cents: 64_000,
      })
    ).bill.id;
    const agents = await bill({ vendor_id: v, invoice_number: 'SL-101', invoice_date: '2026-11-05', amount_cents: 22_500 });

    // The inbox shows his own bill as waiting on someone else, with no Approve.
    await page.goto('/');
    const others = page.getByRole('region', { name: 'Bills waiting on someone else' });
    await expect(others.getByText('SL-100')).toBeVisible();
    await expect(others.getByText('You entered this bill. Waiting on maria.')).toBeVisible();
    await expect(others.getByRole('button', { name: 'Approve' })).toHaveCount(0);
    const toApprove = page.getByRole('region', { name: 'Bills to approve' });
    await expect(toApprove.getByText('SL-101')).toBeVisible();

    // So does the bill page.
    await page.goto(`/bills/${own}`);
    await expect(page.getByText('Waiting on someone else')).toBeVisible();
    await expect(page.getByText('You entered this bill. Waiting on maria.')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Approve' })).toHaveCount(0);
    // The server refuses it anyway, and says who else can.
    await expect(asPerson(page, 'bill.approve', { bill_id: own })).rejects.toThrow(
      /403.*you entered this bill.*waiting on maria/,
    );

    // The agent's bill is his to approve.
    await page.goto(`/bills/${agents}`);
    await page.getByRole('button', { name: 'Approve' }).click();
    await expect(toast(page, 'Approved Saguaro Landscaping SL-101')).toBeVisible();

    // Turned off, his own bill is his to approve again.
    await page.goto('/settings/approvals');
    await page.getByLabel(RULE).click();
    await expect(toast(page, 'Anyone on the step can approve')).toBeVisible();
    await page.goto(`/bills/${own}`);
    await page.getByRole('button', { name: 'Approve' }).click();
    await expect(toast(page, 'Approved Saguaro Landscaping SL-100')).toBeVisible();
  } finally {
    // The browser tests share one database: leave the rule off.
    const now = await agent<{ submitter_cannot_approve: boolean }>('approval_rules.get', {});
    if (now.submitter_cannot_approve) await asPerson(page, 'approval_rules.set', { submitter_cannot_approve: false });
    await asPerson(page, 'person.remove', { actor: 'maria' });
  }
});

test('agents can read the approval rules but not change them', async () => {
  await expect(agent('approval_rules.set', { submitter_cannot_approve: true })).rejects.toThrow(/403|person/);
});
