// The first-run checklist (/start): where `/` lands for a company with setup
// left, on hosted Payvy (trial and plans) and self-hosted (neither).
//
// The run's server is self-hosted and its one company is shared with every
// other test, so what a test needs to be true is given here, as
// billing-trial.spec.ts does: the session's plan and billing.get stand in
// for the management app, and the reads the steps are ticked from (documents,
// bills, bank, people) are answered here where a test needs a company that
// is new or one that is finished. Uploading, and the reads after it, are real.
import fs from 'node:fs';
import { expect, test, type Page } from '@playwright/test';
import { agent } from './api';
import { SIGNED_IN } from './env';
import { pdf } from './pdf';

// Signed in, in a browser that hasn't skipped the checklist.
test.use({
  storageState: async ({}, use) => use({ cookies: JSON.parse(fs.readFileSync(SIGNED_IN, 'utf8')).cookies, origins: [] }),
});

const OFFERS = [
  { id: 'byoa', name: 'Bring your own agent', price: 'Free for 14 days, then $39 a month', description: ['Your agent reads invoices.'] },
  { id: 'managed', name: 'Payvy reads for you', price: '$99 a month, charged today', description: ['Payvy reads every invoice.'] },
];
const ADMIN_ONLY = ['billing.checkout', 'billing.portal', 'bank.connect', 'person.add', 'email.new_address'];
const DAY = 86_400_000;
const SUBSCRIBED = { plan: 'byoa', status: 'active', trial_end: null, current_period_end: '2026-12-28T12:00:00Z', cancel_at_period_end: false, payments_stop_at: null };

const answer = (command: string, result: object) => ({ json: { command, mode: 'commit', events: [], result } });

interface Hosted {
  plan: { provisional?: boolean; reader: boolean; payments: boolean };
  billing: { subscription: object | null; trial: { ends_at: string; running: boolean } | null; offers?: object[] };
  /** Signed in without the Admin role. */
  member?: boolean;
}

/** Hosted Payvy with a management app behind it. `h` is read on every
 *  request, so a test can change the plan as Stripe would. */
async function hosted(page: Page, h: Hosted) {
  await page.route('**/auth/session', async (route) => {
    if (route.request().method() !== 'GET') return route.continue();
    const res = await route.fetch();
    const me = await res.json();
    const commands: string[] = h.member
      ? me.commands.filter((c: string) => !ADMIN_ONLY.includes(c))
      : [...new Set([...me.commands, 'billing.get', 'billing.checkout', 'billing.portal'])];
    await route.fulfill({ response: res, json: { ...me, commands, plan: { hosted: true, billing: true, ...h.plan } } });
  });
  await page.route('**/api/commands/billing.get', (route) =>
    route.fulfill(answer('billing.get', { plan: { hosted: true, ...h.plan }, available: true, offers: OFFERS, ...h.billing })),
  );
}

/** What the steps are ticked from, for a company that is new (`false`) or
 *  finished (`true`). Read on every request. */
async function company(page: Page, c: { bill: boolean; bank: boolean; people: boolean }) {
  await page.route('**/api/commands/document.list', (route) =>
    route.fulfill(answer('document.list', { documents: c.bill ? [{ id: 'd1' }] : [] })));
  await page.route('**/api/commands/bill.list', (route) => route.fulfill(answer('bill.list', { bills: [] })));
  await page.route('**/api/commands/bank.get', (route) =>
    route.fulfill(answer('bank.get', {
      connection: null,
      ramp_available: false,
      connections: c.bank ? [{ id: 'b1', provider: 'mercury', capability: 'pay', mode: 'request', account_name: 'Operating ••1234' }] : [],
    })));
  await page.route('**/api/commands/person.list', (route) =>
    route.fulfill(answer('person.list', { people: c.people ? [{ actor: 'robert' }, { actor: 'dana' }] : [{ actor: 'robert' }] })));
}

const step = (page: Page, title: string, done: boolean) =>
  page.getByRole('listitem', { name: `${title}: ${done ? 'done' : 'not done'}`, exact: true });
const inTrial = (days: number): Hosted => ({
  plan: { reader: false, payments: true },
  billing: { subscription: null, trial: { ends_at: new Date(Date.now() + days * DAY - 3_600_000).toISOString(), running: true } },
});

test('a new company in its trial lands on the checklist and uploads its first bill', async ({ page }, info) => {
  const c = { bill: false, bank: false, people: false };
  await hosted(page, inTrial(12));
  await company(page, c);
  // No address yet; creating one is answered here so the run's real address stays.
  let address: string | null = null;
  await page.route('**/api/commands/email.address', (route) => route.fulfill(answer('email.address', { address })));
  await page.route('**/api/commands/email.new_address', (route) => {
    address = 'k7m2p9x4q6wd@in.payvy.test';
    return route.fulfill(answer('email.new_address', { address, retired: null }));
  });
  const reads: string[] = [];
  page.on('request', (r) => { if (r.url().endsWith('/api/commands/document.extract')) reads.push(r.url()); });

  await page.goto('/');
  await expect(page).toHaveURL(/\/start$/);
  await expect(page.getByRole('heading', { level: 1 })).toHaveText(/ is ready$/);
  await expect(page.getByText('Your company was created. Here is what to do first.')).toBeVisible();
  await expect(page.getByText('Trial: 12 days left.', { exact: true })).toBeVisible();
  await expect(page.getByText('No card needed until then.', { exact: true })).toBeVisible();

  // Four steps, in order, none done.
  await expect(page.locator('.payvy-setup > li')).toHaveCount(4);
  const titles = ['Send the first bill', 'Connect the bank that pays', 'Invite the people who approve', 'Choose a plan'];
  await expect(page.locator('.payvy-setup-step-body > h2')).toHaveText(titles);
  for (const t of titles) await expect(step(page, t, false)).toBeVisible();

  // The plans are the Billing page's cards, with the management app's terms.
  const plan = step(page, 'Choose a plan', false);
  await expect(plan.getByText('Not needed during the trial: 12 days left.')).toBeVisible();
  await expect(plan.getByText('Free for 14 days, then $39 a month', { exact: true })).toBeVisible();
  await expect(plan.getByText('$99 a month, charged today', { exact: true })).toBeVisible();
  await expect(plan.getByRole('button', { name: 'Choose Bring your own agent', exact: true })).toBeVisible();
  await expect(plan.getByRole('button', { name: 'Choose Payvy reads for you', exact: true })).toBeVisible();

  // Payvy's own reading isn't in the trial: said once, with the plan that has it.
  const first = step(page, 'Send the first bill', false);
  await expect(first.getByRole('link', { name: 'Included in Payvy reads for you ($99 a month, charged today)', exact: true })).toHaveAttribute('href', '/settings/billing');

  // The email-in address: none until a person creates it.
  await expect(first.getByText('No email-in address yet.')).toBeVisible();
  await first.getByRole('button', { name: 'Create email-in address', exact: true }).click();
  await expect(first.getByLabel('Email-in address', { exact: true })).toHaveText('k7m2p9x4q6wd@in.payvy.test');
  await expect(first.getByRole('button', { name: 'Copy', exact: true })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Get started', exact: true })).toBeVisible();
  await page.screenshot({ path: info.outputPath('first-run-trial.png'), fullPage: true, animations: 'disabled' });

  // The upload is real. With the reader off nothing is asked to read it.
  await page.unroute('**/api/commands/document.list');
  // A file of its own: the store refuses a second copy of the same bytes.
  await first.locator('input[type=file]').setInputFiles({
    name: 'first-bill.pdf',
    mimeType: 'application/pdf',
    buffer: pdf(['Juniper Supply', 'Invoice FR-1001', `Ref ${Date.now()}`, 'Date 11/03/2026', 'Total 64.00']),
  });
  await expect(page).toHaveURL(/\/inbox\/[0-9a-f-]{36}$/);
  await expect(page.getByText('Not read yet')).toBeVisible();
  expect(reads).toEqual([]);
  // The review page offers no reading it would refuse.
  await expect(page.getByRole('button', { name: 'Read it', exact: true })).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'Included in Payvy reads for you ($99 a month, charged today)', exact: true })).toBeVisible();

  // Back on the checklist the step is ticked, from the document that exists.
  await page.getByRole('link', { name: 'Get started', exact: true }).click();
  await expect(step(page, 'Send the first bill', true)).toBeVisible();
  await expect(step(page, 'Connect the bank that pays', false)).toBeVisible();
});

test('choosing a plan goes to Checkout and comes back to the checklist, ticked when the plan shows', async ({ page }) => {
  const h = inTrial(9);
  await hosted(page, h);
  await company(page, { bill: true, bank: false, people: false });
  let asked: unknown = null;
  await page.route('**/api/commands/billing.checkout', (route) => {
    asked = route.request().postDataJSON().input;
    return route.fulfill(answer('billing.checkout', { kind: 'checkout', url: 'https://checkout.example/c/1' }));
  });
  await page.route('https://checkout.example/**', (route) => route.fulfill({ contentType: 'text/html', body: '<h1>Checkout</h1>' }));

  await page.goto('/start');
  await step(page, 'Choose a plan', false).getByRole('button', { name: 'Choose Bring your own agent', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Checkout' })).toBeVisible();
  // The server is asked to bring the person back here, not to Billing.
  expect(asked).toEqual({ plan: 'byoa', return_to: 'start' });

  // Stripe sends the browser back before its confirmation reaches the plan.
  await page.goto('/start');
  await expect(step(page, 'Choose a plan', false)).toBeVisible();
  await expect(page.getByText('Waiting for Stripe to confirm the plan')).toBeVisible();
  h.billing = { subscription: SUBSCRIBED, trial: { ends_at: h.billing.trial!.ends_at, running: false } };
  const done = step(page, 'Choose a plan', true);
  await expect(done).toBeVisible();
  await expect(done.getByText('Bring your own agent')).toBeVisible();
  await expect(done.getByRole('button')).toHaveCount(0);
  await expect(page.getByText('Waiting for Stripe to confirm the plan')).toHaveCount(0);
  await expect(page.getByText(/^Trial:/)).toHaveCount(0);
});

test('a company whose trial ended with no plan lands on Billing', async ({ page }) => {
  await hosted(page, {
    plan: { reader: false, payments: false },
    billing: { subscription: null, trial: { ends_at: '2026-11-01T12:00:00Z', running: false } },
  });
  await company(page, { bill: false, bank: false, people: false });
  await page.goto('/');
  await expect(page).toHaveURL(/\/settings\/billing$/);
  await expect(page.getByText('Your trial ended on Nov 1, 2026.', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Choose Bring your own agent', exact: true })).toBeVisible();
});

test('a new company whose plan has not reached the server yet still lands on the checklist', async ({ page }) => {
  // The management app started the trial; its push of the plan hasn't landed.
  await hosted(page, { ...inTrial(14), plan: { provisional: true, reader: false, payments: false } });
  await company(page, { bill: false, bank: false, people: false });
  await page.goto('/');
  await expect(page).toHaveURL(/\/start$/);
  await expect(page.getByText('Trial: 14 days left.', { exact: true })).toBeVisible();
});

test('a member without the Admin role sees the steps but no plan buttons', async ({ page }) => {
  await hosted(page, { ...inTrial(5), member: true });
  await company(page, { bill: false, bank: false, people: false });
  await page.goto('/');
  // They can upload, so they land here too.
  await expect(page).toHaveURL(/\/start$/);
  await expect(page.getByRole('button', { name: 'Upload a bill', exact: true })).toBeVisible();
  const plan = step(page, 'Choose a plan', false);
  await expect(plan.getByText('Ask an admin to choose a plan.', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: /^Choose / })).toHaveCount(0);
  await expect(page.getByText('$99 a month, charged today', { exact: true })).toHaveCount(0);
  await expect(page.getByText('Ask an admin to connect it.')).toBeVisible();
  await expect(page.getByText('Ask an admin to add them.')).toBeVisible();
  await expect(page.getByRole('link', { name: 'Connect Mercury' })).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'Add people' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Create email-in address' })).toHaveCount(0);
  // The locked reading is a sentence for them, not a link to settings they can't open.
  await expect(page.getByText('Included in Payvy reads for you ($99 a month, charged today). Ask an admin to change the plan.')).toBeVisible();
});

test('a company with a plan and every step done never sees the checklist', async ({ page }) => {
  await hosted(page, { plan: { reader: false, payments: true }, billing: { subscription: SUBSCRIBED, trial: { ends_at: '2026-11-01T12:00:00Z', running: false } } });
  await company(page, { bill: true, bank: true, people: true });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole('link', { name: 'Get started', exact: true })).toHaveCount(0);
});

test('a plan granted without a subscription or a trial counts as chosen', async ({ page }) => {
  // A company the operator entitled by hand: nothing for it to choose.
  await hosted(page, { plan: { reader: true, payments: true }, billing: { subscription: null, trial: null } });
  await company(page, { bill: true, bank: true, people: true });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Get started', exact: true })).toHaveCount(0);
});

test('self-hosted: three steps, no trial and no plans; skipping is remembered here', async ({ page }, info) => {
  await company(page, { bill: false, bank: false, people: false });
  await page.goto('/');
  await expect(page).toHaveURL(/\/start$/);
  await expect(page.getByRole('heading', { level: 1 })).toHaveText(/ is ready$/);
  await expect(page.locator('.payvy-setup > li')).toHaveCount(3);
  await expect(page.locator('.payvy-setup-step-body > h2')).toHaveText(['Send the first bill', 'Connect the bank that pays', 'Invite the people who approve']);
  await expect(page.getByText(/Trial/)).toHaveCount(0);
  await expect(page.getByText(/plan/i)).toHaveCount(0);
  await expect(page.getByText(/Included in/)).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Upload a bill', exact: true })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Connect Mercury', exact: true })).toHaveAttribute('href', '/settings/bank');
  await expect(page.getByRole('link', { name: 'Add people', exact: true })).toHaveAttribute('href', '/settings/people');
  // The run's server takes email: the real address, or the offer to create it.
  const real = await agent<{ address: string | null }>('email.address', {});
  if (real.address) await expect(page.getByLabel('Email-in address', { exact: true })).toHaveText(real.address);
  else await expect(page.getByRole('button', { name: 'Create email-in address', exact: true })).toBeVisible();
  await page.screenshot({ path: info.outputPath('first-run-self-hosted.png'), fullPage: true, animations: 'disabled' });

  // The menu's Waiting on you is never the checklist.
  await page.getByRole('link', { name: 'Waiting on you' }).click();
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  // Arriving again is: nothing was skipped.
  await page.goto('/');
  await expect(page).toHaveURL(/\/start$/);

  await page.getByRole('button', { name: 'Skip for now', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  await expect(page).toHaveURL(/\/$/);
  // Skipped, not done: the menu still has it, and no step was ticked by it.
  await page.getByRole('link', { name: 'Get started', exact: true }).click();
  await expect(step(page, 'Send the first bill', false)).toBeVisible();
});

test('with every step done the checklist leaves the menu, and says so if opened', async ({ page }) => {
  await company(page, { bill: true, bank: true, people: true });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Get started', exact: true })).toHaveCount(0);
  await page.goto('/start');
  await expect(page.getByText('Everything here is done')).toBeVisible();
  await expect(page.locator('.payvy-setup > li')).toHaveCount(3);
  await expect(step(page, 'Connect the bank that pays', true)).toContainText('Mercury · Operating ••1234');
  await expect(page.getByRole('button', { name: 'Skip for now' })).toHaveCount(0);
});

test('a read that fails never sends anyone to the checklist', async ({ page }) => {
  await page.route('**/api/commands/person.list', (route) => route.fulfill({ status: 500, json: { error: { code: 'internal', message: 'boom' } } }));
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  await expect(page).toHaveURL(/\/$/);
});
