// A vendor that keeps one of the company's cards: a person links the card on
// the vendor page, Payvy reads its charges from the (fake) bank, and a person
// decides the charges Payvy can't pair with a bill on its own.
import { expect, test, type Page } from '@playwright/test';
import { agent, bill, state, vendor } from './api';

async function personCommand(page: Page, command: string, input: object) {
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
}

async function charge(id: string, dollars: number) {
  await fetch(`${state().mercuryUrl}/test/charge`, {
    method: 'POST',
    body: JSON.stringify({ id, cardId: 'card-bins', amount: -dollars, at: new Date().toISOString() }),
  });
}

test('a person links the card a vendor keeps, then matches and dismisses its charges', async ({ page }) => {
  const id = await vendor('Card vendor');
  await expect(agent('vendor.link_card', { vendor_id: id, card_id: 'card-bins' })).rejects.toThrow(/403|person/);
  await personCommand(page, 'bank.connect', { provider: 'mercury', environment: 'sandbox', token: 'fake-mercury-token' });
  try {
    await page.goto(`/vendors/${id}`);
    await expect(page.getByRole('heading', { name: 'Card on file' })).toBeVisible();
    await page.getByRole('button', { name: 'Link a card…' }).click();
    await page.getByRole('combobox', { name: 'Card at the bank' }).click();
    await page.getByRole('option', { name: /••4242 · Bins/ }).click();
    await page.getByRole('button', { name: 'Link card' }).click();
    await expect(page.getByRole('button', { name: 'Link card' })).toHaveCount(0);
    await expect(page.getByRole('definition').filter({ hasText: '••4242 · Bins · credit' })).toBeVisible();
    await expect(page.getByText(/Releasing a bill sends no payment/)).toBeVisible();

    // Two bills of $750: a $750 charge could be either, so a person says which.
    const today = new Date().toISOString().slice(0, 10);
    const first = await bill({ vendor_id: id, invoice_number: 'CARD-1', invoice_date: today, amount_cents: 75_000 });
    await bill({ vendor_id: id, invoice_number: 'CARD-2', invoice_date: today, amount_cents: 75_000 });
    await charge('card-txn-1', 750);
    await charge('card-txn-fee', 35);
    await agent('card.sync', {});
    await page.reload();
    await expect(page.getByText(/match it to a bill, or dismiss it/)).toHaveCount(2);

    // Newest first: the fee, then the $750. Only open charges offer a choice.
    await page.getByRole('button', { name: 'Match to a bill…' }).last().click();
    await page.getByRole('combobox', { name: 'Bill this charge paid' }).click();
    await page.getByRole('option', { name: /CARD-1/ }).click();
    await page.getByRole('button', { name: 'Match', exact: true }).click();
    await expect(page.getByRole('link', { name: 'CARD-1' })).toBeVisible();

    await page.getByRole('button', { name: 'Dismiss…' }).click();
    await page.getByLabel('Why it pays no bill').fill('late fee, disputed');
    await page.getByRole('button', { name: 'Dismiss', exact: true }).click();
    await expect(page.getByText('Dismissed: late fee, disputed')).toBeVisible();
    await expect(page.getByText(/match it to a bill, or dismiss it/)).toHaveCount(0);

    const matched = await agent<{ card_charge: { transaction_id: string; rule: string } }>('bill.get', { bill_id: first });
    expect(matched.card_charge).toMatchObject({ transaction_id: 'card-txn-1', rule: 'person' });

    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.getByRole('heading', { name: 'Card on file' }).scrollIntoViewIfNeeded();
    await page.screenshot({ path: 'test-results/card-on-file-mobile.png', fullPage: true });
  } finally {
    await personCommand(page, 'bank.disconnect', { provider: 'mercury' });
  }
});
