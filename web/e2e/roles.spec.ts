import { expect, test, type Page } from '@playwright/test';
import { agent, bill, state, vendor } from './api';
import { BASE_URL } from './env';
import { linkAfter, logMark } from './mail';

async function command(page: Page, name: string, input: object) {
  const r = await page.request.post(`/api/commands/${name}`, { headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input } });
  expect(r.ok(), await r.text()).toBe(true);
  return (await r.json()).result;
}
async function choose(page: Page, name: string) {
  await page.getByRole('combobox', { name: 'Roles', exact: true }).click();
  await page.getByRole('option', { name, exact: true }).click();
  await page.keyboard.press('Escape');
}

test('roles combine, restrict real sessions, and update without signing in again', async ({ page, browser }, info) => {
  test.setTimeout(60_000);
  await page.goto('/settings/people');
  await page.getByRole('button', { name: 'Add person', exact: true }).click();
  await page.getByRole('textbox', { name: 'Name Required', exact: true }).fill('Role Clerk');
  await page.getByRole('textbox', { name: 'Email Required', exact: true }).fill('role-clerk@example.com');
  await expect(page.getByRole('combobox', { name: 'Roles', exact: true })).toContainText('AP clerk');
  await page.getByRole('dialog').getByRole('button', { name: 'Add person', exact: true }).click();
  await expect(page.getByText('Role Clerk can sign in now')).toBeVisible();

  const ctx = await browser.newContext({ baseURL: BASE_URL, storageState: { cookies: [], origins: [] } });
  const clerk = await ctx.newPage();
  try {
    const mark = logMark(state().serverLog);
    await clerk.goto('/sign-in');
    await clerk.getByLabel('Work email').fill('role-clerk@example.com');
    await clerk.getByRole('button', { name: 'Email me a link' }).click();
    await clerk.goto(await linkAfter(state().serverLog, mark));
    await expect(clerk.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
    await clerk.goto('/settings/people');
    await expect(clerk.getByRole('button', { name: 'Add person', exact: true })).toHaveCount(0);
    await expect(clerk.getByRole('button', { name: /Edit roles for/ })).toHaveCount(0);
    await clerk.goto('/settings/company');
    await expect(clerk.getByText("You don't have access to these settings")).toBeVisible();
    const denied = await clerk.request.post('/api/commands/person.set_roles', { headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input: { actor: 'role-clerk', roles: ['admin'] } } });
    expect(denied.status()).toBe(403);
    await clerk.goto('/inbox');
    await expect(clerk.getByRole('button', { name: 'Add invoices' })).toBeVisible();

    // A workflow can't name someone who can't approve: an AP clerk is refused by name.
    const v = await vendor('Role Workflow Vendor');
    const steps = [
      { id: 'approval', type: 'approve', approvers: ['role-clerk'] },
      { id: 'payment', type: 'pay', by: ['role-clerk'] },
    ];
    await expect(agent('workflow.propose', { vendor_id: v, workflow: { steps } })).rejects.toThrow(/role-clerk can't approve bills/);

    await page.getByRole('button', { name: 'Edit roles for Role Clerk', exact: true }).click();
    await choose(page, 'AP clerk');
    await choose(page, 'Approver');
    await choose(page, 'Payment releaser');
    await page.screenshot({ path: info.outputPath('roles-editor.png'), animations: 'disabled' });
    await page.getByRole('button', { name: 'Save roles', exact: true }).click();
    await expect(page.getByRole('dialog')).toHaveCount(0);

    // Returning to an already-open page refreshes access without reloading it.
    await clerk.evaluate(() => window.dispatchEvent(new Event('focus')));
    await expect(clerk.getByRole('button', { name: 'Add invoices' })).toHaveCount(0);

    // Now an approver, the same workflow is accepted, and they approve and can pay.
    const wf = await agent<{ proposal_id: string }>('workflow.propose', { vendor_id: v, workflow: { steps } });
    await command(page, 'workflow.accept', { proposal_id: wf.proposal_id });
    const id = await bill({ vendor_id: v, invoice_number: 'ROLE-1', invoice_date: '2026-11-10', amount_cents: 12300 });
    await clerk.goto(`/bills/${id}`);
    await clerk.getByRole('button', { name: 'Approve', exact: true }).click();
    await expect(clerk.getByRole('button', { name: 'Pay…', exact: true })).toBeVisible();
    await clerk.goto('/vendors');
    await expect(clerk.getByRole('button', { name: 'Add vendor', exact: true })).toHaveCount(0);

    await page.setViewportSize({ width: 390, height: 844 });
    await page.screenshot({ path: info.outputPath('people-roles-mobile.png'), fullPage: true });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  } finally {
    await ctx.close();
    await command(page, 'person.remove', { actor: 'role-clerk' });
  }
});

test('the last admin cannot demote themselves in People', async ({ page }) => {
  await page.goto('/settings/people');
  await page.getByRole('button', { name: 'Edit roles for Robert', exact: true }).click();
  await choose(page, 'Approver');
  await choose(page, 'Admin');
  await page.getByRole('button', { name: 'Save roles', exact: true }).click();
  await expect(page.getByText(/last admin who can sign in/)).toBeVisible();
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
});
