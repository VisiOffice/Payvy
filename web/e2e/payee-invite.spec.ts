import { expect, test, type Page } from '@playwright/test';
import { agent, state, vendor } from './api';

async function personCommand(page: Page, command: string, input: object) {
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
}

test('vendor invite previews, sends once, and still needs payee acceptance', async ({ page }) => {
  const id = await vendor('Invited vendor');
  const input = { vendor_id: id, email: 'vendor@example.com', method: 'ach' };
  await expect(agent('vendor.invite_payee', input)).rejects.toThrow(/403|person/);
  await personCommand(page, 'bank.connect', { provider: 'mercury', environment: 'sandbox', token: 'fake-mercury-token' });
  try {
    await page.goto(`/vendors/${id}`);
    await page.getByRole('button', { name: 'Invite payment details…' }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByLabel('Vendor email').fill('vendor@example.com');
    await dialog.getByRole('button', { name: 'Review invitation' }).click();
    await expect(dialog.getByText(/Mercury will email vendor@example.com/)).toBeVisible();
    expect((await (await fetch(`${state().mercuryUrl}/test/posts`)).json()).posts).toBe(0);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.screenshot({ path: 'test-results/payee-invite-review-mobile.png' });
    await dialog.getByRole('button', { name: 'Send invitation' }).click();
    await page.setViewportSize({ width: 1440, height: 900 });
    await expect(page.getByText('Waiting for the vendor', { exact: true })).toBeVisible();
    expect((await (await fetch(`${state().mercuryUrl}/test/posts`)).json()).posts).toBe(1);
    await expect(page.getByRole('button', { name: 'Invite payment details…' })).toHaveCount(0);
    await expect(page.getByText(/Payvy checks with Mercury every few minutes/)).toBeVisible();
    await page.reload();
    await expect(page.getByText('Waiting for the vendor', { exact: true })).toBeVisible();
    await fetch(`${state().mercuryUrl}/test/complete`, { method: 'POST' });
    await page.getByRole('button', { name: 'Check now' }).click();
    await expect(page.getByText('Change proposed', { exact: true })).toBeVisible();
    await expect(page.getByText(/…6789/)).toBeVisible();
    expect(await page.locator('body').innerText()).not.toContain('000123456789');
    const detail = await agent<{ payees: { status: string; id: string }[] }>('vendor.get', { vendor_id: id });
    expect(detail.payees).toHaveLength(1);
    expect(detail.payees[0]!.status).toBe('proposed');
    await expect(agent('vendor.accept_payee', { proposal_id: detail.payees[0]!.id })).rejects.toThrow(/403|person/);
    await page.getByRole('link', { name: 'Review in Waiting on you' }).click();
    const card = page.locator('div').filter({ has: page.getByText('Invited vendor', { exact: true }) })
      .filter({ has: page.getByRole('button', { name: 'Accept' }) }).last();
    await card.getByRole('button', { name: 'Accept' }).click();
    await page.goto(`/vendors/${id}`);
    await expect(page.getByText('Accepted by', { exact: true })).toBeVisible();
    await expect(page.getByText('Payment details accepted', { exact: true })).toBeVisible();
    await expect(page.getByText('Review the proposed destination in Waiting on you before paying this vendor.')).toHaveCount(0);
    // Also exercise the compact layout for the new payment-details panel.
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByRole('button', { name: 'Invite payment details…' })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.getByRole('button', { name: 'Invite payment details…' }).scrollIntoViewIfNeeded();
    await page.screenshot({ path: 'test-results/payee-invite-mobile.png', fullPage: true });
  } finally {
    await personCommand(page, 'bank.disconnect', { provider: 'mercury' });
  }
});
