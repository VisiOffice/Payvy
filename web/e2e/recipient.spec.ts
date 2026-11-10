import { expect, test, type Page } from '@playwright/test';
import { agent, state, vendor } from './api';

async function personCommand(page: Page, command: string, input: object) {
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
}

test('a person enters bank details, Mercury creates the recipient, and it still needs accepting', async ({ page }) => {
  const id = await vendor('Typed-in vendor');
  await personCommand(page, 'bank.connect', { provider: 'mercury', environment: 'sandbox', token: 'fake-mercury-token' });
  try {
    await page.goto(`/vendors/${id}`);
    await page.getByRole('button', { name: 'Enter bank details…' }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByLabel('Vendor email').fill('ar@typed.example');
    await dialog.getByLabel('Routing number').fill('121000248');
    await dialog.getByLabel('Account number').fill('000987654321');
    await dialog.getByLabel('Legal address').fill('1 Main St');
    await dialog.getByLabel('City').fill('Mesa');
    await dialog.getByLabel('State').fill('AZ');
    await dialog.getByLabel('ZIP').fill('85201');
    await dialog.getByRole('button', { name: 'Review' }).click();
    await expect(dialog.getByText(/to …4321/)).toBeVisible();
    expect((await (await fetch(`${state().mercuryUrl}/test/posts`)).json()).recipientPosts).toBe(0);
    await dialog.getByRole('button', { name: 'Create in Mercury' }).click();
    await expect(dialog).toHaveCount(0);
    expect((await (await fetch(`${state().mercuryUrl}/test/posts`)).json()).recipientPosts).toBe(1);
    expect(await page.locator('body').innerText()).not.toContain('000987654321');
    const detail = await agent<{ payees: { status: string; destination: string }[] }>('vendor.get', { vendor_id: id });
    expect(detail.payees).toHaveLength(1);
    expect(detail.payees[0]!.status).toBe('proposed');
    expect(detail.payees[0]!.destination).toBe('…4321');
  } finally {
    await personCommand(page, 'bank.disconnect', { provider: 'mercury' });
  }
});
