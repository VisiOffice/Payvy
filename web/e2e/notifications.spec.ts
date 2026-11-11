import { expect, test, type Page } from '@playwright/test';
import fs from 'node:fs';
import { agent, bill, state, vendor } from './api';
import { BASE_URL, PERSON } from './env';
import { linkAfter, logMark } from './mail';
async function command(page: Page, name: string, input: object) {
  const r = await page.request.post(`/api/commands/${name}`, { headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input } });
  expect(r.ok(), await r.text()).toBe(true);
  return (await r.json()).result;
}

// The minute worker may win the race in a slower CI run. Both paths use the
// same commands and unique notice; accepting a conflict must not trigger a resend.
async function queueAndDeliver(page: Page, billId: string, step: string) {
  async function attempt(name: string, input: object) {
    const r = await page.request.post(`/api/commands/${name}`, { headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input } });
    expect([200, 409], await r.text()).toContain(r.status());
  }
  async function notice() {
    const data = await command(page, 'notification.list', { bill_id: billId });
    return (data.notifications as { id: string; step_id: string; actor: string; status: string }[]).find(n => n.step_id === step && n.actor === 'robert');
  }
  await attempt('notification.queue', { bill_id: billId });
  const n = await notice();
  expect(n).toBeDefined();
  if (n?.status === 'queued') await attempt('notification.deliver', { notification_id: n.id });
  await expect.poll(async () => (await notice())?.status).toBe('logged');
}

test('opt in, deliver an actionable notice, sign in to its bill, and read delivery history', async ({ page, browser }, info) => {
  await page.goto('/settings/notifications');
  await expect(page.getByRole('heading', { name: 'Notifications', exact: true })).toBeVisible();
  await expect(page.getByText('Development mode: emails are logged')).toBeVisible();
  const enabled = page.getByRole('switch', { name: 'Email people when a bill needs them', exact: true });
  await expect(enabled).not.toBeChecked();
  await enabled.click();
  await expect(enabled).toBeChecked();
  await page.screenshot({ path: info.outputPath('notification-settings.png'), fullPage: true });
  const ctx = await browser.newContext({ baseURL: BASE_URL, storageState: { cookies: [], origins: [] } });
  try {
    const v = await vendor('Notification Test Vendor');
    const wf = await agent<{ proposal_id: string }>('workflow.propose', { vendor_id: v, workflow: { steps: [
      { id: 'review', type: 'approve', approvers: ['robert'] }, { id: 'pay', type: 'pay', by: ['robert'] },
    ] } });
    await command(page, 'workflow.accept', { proposal_id: wf.proposal_id });
    const log = state().serverLog;
    const mark = logMark(log);
    const id = await bill({ vendor_id: v, invoice_number: 'NOTICE-1', invoice_date: '2026-11-10', amount_cents: 12500 });
    await queueAndDeliver(page, id, 'review');
    const mail = fs.readFileSync(log, 'latin1').slice(mark);
    expect(mail).toContain('Approval needed');
    expect(mail).toContain(`${BASE_URL}/bills/${id}`);
    expect(mail).toContain('This email does not approve or pay');
    const reader = await ctx.newPage();
    await reader.goto(`${BASE_URL}/bills/${id}`);
    await expect(reader.getByRole('heading', { name: 'Sign in to Payvy' })).toBeVisible();
    const signInMark = logMark(log);
    await reader.getByLabel('Work email').fill(PERSON.email);
    await reader.getByRole('button', { name: 'Email me a link' }).click();
    await reader.goto(await linkAfter(log, signInMark));
    await expect(reader).toHaveURL(`${BASE_URL}/bills/${id}`);
    await expect(reader.getByRole('button', { name: 'Approve', exact: true })).toBeVisible();
    await reader.getByText('Email notifications', { exact: true }).click();
    await expect(reader.getByText('Logged only', { exact: true })).toBeVisible();
    await expect(reader.getByText(/robert@example.com · review/)).toBeVisible();
    await reader.screenshot({ path: info.outputPath('notification-history.png'), fullPage: true });
    await reader.getByRole('button', { name: 'Approve', exact: true }).click();
    await expect(reader.getByRole('button', { name: 'Pay…', exact: true })).toBeVisible();
    await queueAndDeliver(page, id, 'pay');
    await reader.getByRole('button', { name: 'Refresh notifications' }).click();
    await expect(reader.getByText('Ready for payment release', { exact: true })).toBeVisible();
    await page.setViewportSize({ width: 390, height: 844 });
    await page.screenshot({ path: info.outputPath('notification-settings-mobile.png'), fullPage: true });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  } finally {
    await ctx.close();
    await command(page, 'notification.configure', { enabled: false, reminders: true });
  }
});
