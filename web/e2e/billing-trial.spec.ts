// Settings > Billing on hosted Payvy, for a company in or after the
// card-free trial the management app gives at signup. The run's server is
// self-hosted, with no management app behind it, so the two answers the page
// reads are given here: the session's plan, and billing.get as the server
// passes it on (crates/payvy-server/tests/billing.rs covers that side).
import { expect, test, type Page } from '@playwright/test';

const OFFERS = [
  { id: 'byoa', name: 'Bring your own agent', price: 'Free for 14 days, then $39 a month', description: ['Your agent reads invoices.'] },
  { id: 'managed', name: 'Payvy reads for you', price: '$99 a month, charged today', description: ['Payvy reads every invoice.'] },
];

async function hosted(page: Page, plan: { provisional?: boolean; reader: boolean; payments: boolean }, billing: object) {
  await page.route('**/auth/session', async (route) => {
    if (route.request().method() !== 'GET') return route.continue();
    const res = await route.fetch();
    const me = await res.json();
    await route.fulfill({
      response: res,
      json: {
        ...me,
        commands: [...new Set([...me.commands, 'billing.get', 'billing.checkout', 'billing.portal'])],
        plan: { hosted: true, billing: true, ...plan },
      },
    });
  });
  await page.route('**/api/commands/billing.get', (route) =>
    route.fulfill({ json: { command: 'billing.get', mode: 'commit', events: [], result: { plan: { hosted: true, ...plan }, available: true, ...billing } } }),
  );
}

// Noon UTC, so the date reads the same in any time zone the browser is in.
const ENDS = '2026-11-28T12:00:00Z';

test('in the trial, Billing says until when and offers the plans with their terms', async ({ page }, info) => {
  await hosted(page, { reader: false, payments: true }, { subscription: null, trial: { ends_at: ENDS, running: true }, offers: OFFERS });
  await page.goto('/settings/billing');
  await expect(page.getByText('Trial until Nov 28, 2026.', { exact: true })).toBeVisible();
  await expect(page.getByText('Add a plan to keep paying bills after that.', { exact: true })).toBeVisible();
  await expect(page.getByText('Free for 14 days, then $39 a month', { exact: true })).toBeVisible();
  await expect(page.getByText('$99 a month, charged today', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Choose Bring your own agent', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Choose Payvy reads for you', exact: true })).toBeVisible();
  await expect(page.getByText(/Your trial ended/)).toHaveCount(0);
  await expect(page.getByText('Choose a plan to start')).toHaveCount(0);
  await expect(page.getByText('Your plan has ended')).toHaveCount(0);
  await page.screenshot({ path: info.outputPath('billing-trial.png'), animations: 'disabled' });
});

test('the trial notice shows even before the plan reaches the server', async ({ page }) => {
  // The management app's push hasn't landed: the company is still provisional.
  await hosted(page, { provisional: true, reader: false, payments: false }, { subscription: null, trial: { ends_at: ENDS, running: true }, offers: OFFERS });
  await page.goto('/settings/billing');
  await expect(page.getByText('Trial until Nov 28, 2026.', { exact: true })).toBeVisible();
  await expect(page.getByText('Choose a plan to start')).toHaveCount(0);
});

test('after the trial with no plan, Billing says when it ended', async ({ page }, info) => {
  const offers = OFFERS.map((o) => (o.id === 'byoa' ? { ...o, price: '$39 a month, charged today' } : o));
  await hosted(page, { reader: false, payments: false }, { subscription: null, trial: { ends_at: ENDS, running: false }, offers });
  await page.goto('/settings/billing');
  await expect(page.getByText('Your trial ended on Nov 28, 2026.', { exact: true })).toBeVisible();
  await expect(page.getByText('Choose a plan to send payments again.', { exact: true })).toBeVisible();
  await expect(page.getByText(/Trial until/)).toHaveCount(0);
  await expect(page.getByText('Your plan has ended')).toHaveCount(0);
  await expect(page.getByText(/Free for/)).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Choose Bring your own agent', exact: true })).toBeVisible();
  await page.screenshot({ path: info.outputPath('billing-trial-ended.png'), animations: 'disabled' });
});

test('a subscriber, and a company given no trial, see neither notice', async ({ page }) => {
  // Subscribed during the trial: the subscription decides, so the trial isn't running.
  await hosted(page, { reader: false, payments: true }, {
    subscription: { plan: 'byoa', status: 'trialing', trial_end: ENDS, current_period_end: ENDS, cancel_at_period_end: false, payments_stop_at: null },
    trial: { ends_at: ENDS, running: false },
    offers: OFFERS.map((o) => ({ ...o, price: o.id === 'byoa' ? '$39 a month' : '$99 a month' })),
  });
  await page.goto('/settings/billing');
  await expect(page.getByRole('button', { name: 'Manage billing', exact: true })).toBeVisible();
  await expect(page.getByText(/Trial until|Your trial ended/)).toHaveCount(0);

  // An older management app, or a company it gave no trial: no `trial` at all.
  await page.unroute('**/api/commands/billing.get');
  await page.route('**/api/commands/billing.get', (route) =>
    route.fulfill({ json: { command: 'billing.get', mode: 'commit', events: [], result: { plan: { hosted: true, provisional: true, reader: false, payments: false }, available: true, subscription: null, offers: OFFERS } } }),
  );
  await page.reload();
  await expect(page.getByText('Choose a plan to start', { exact: true })).toBeVisible();
  await expect(page.getByText(/Trial until|Your trial ended/)).toHaveCount(0);
});
