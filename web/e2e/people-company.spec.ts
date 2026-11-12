import fs from 'node:fs';
import { expect, test, type Page } from '@playwright/test';
import { agent, state } from './api';
import { BASE_URL } from './env';
import { linkAfter, logMark } from './mail';

/** A command only a person may run, with the signed-in browser's cookie. */
async function personCommand(page: Page, command: string, input: object) {
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
}

async function actors(): Promise<string[]> {
  return (await agent<{ people: { actor: string }[] }>('person.list', {})).people.map((p) => p.actor);
}

test('a person adds someone, who signs in with a link, then removes them', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Default, company menu', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Invite and manage people' }).click();
  await expect(page).toHaveURL(/\/settings\/people$/);
  await expect(page.getByRole('heading', { name: 'People', level: 1 })).toBeVisible();
  await expect(page.getByText('You', { exact: true })).toBeVisible();
  // Alone here: nobody can be removed, least of all yourself.
  await expect(page.getByRole('button', { name: /^Remove / })).toHaveCount(0);
  await expect(page.getByText(/You're the only person here/)).toBeVisible();

  const log = state().serverLog;
  const mark = logMark(log);
  await page.getByRole('button', { name: 'Add person' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText("Payvy doesn't send anything now");
  await dialog.getByLabel('Name').fill('Dana Whitfield');
  await dialog.getByLabel('Email').fill('Dana@Example.com');
  await expect(dialog.getByLabel('Recorded as')).toHaveAttribute('placeholder', 'dana');
  await dialog.getByRole('button', { name: 'Add person' }).click();
  await expect(dialog).toHaveCount(0);

  await expect(page.getByText('Dana Whitfield can sign in now')).toBeVisible();
  await expect(page.getByText(/Payvy doesn't email them\. Tell them to open .* and enter dana@example\.com/)).toBeVisible();
  // Adding someone sends nothing: no mail reached the log.
  expect(fs.readFileSync(log, 'latin1').slice(mark)).not.toContain('dana@example.com');
  await expect(page.getByText('dana@example.com', { exact: true })).toBeVisible();
  await expect(page.getByText(/Added .* by robert/).first()).toBeVisible();
  expect(await actors()).toContain('dana');

  // They sign in the usual way: ask for a link, which arrives by email.
  const before = logMark(log);
  await fetch(`${BASE_URL}/auth/link`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ email: 'dana@example.com' }),
  });
  expect(await linkAfter(log, before)).toMatch(/\/sign-in#token=/);

  // You still can't remove yourself; an agent can't remove anyone.
  await expect(page.getByRole('button', { name: 'Remove Robert' })).toHaveCount(0);
  await expect(agent('person.remove', { actor: 'dana' })).rejects.toThrow(/403/);

  await page.getByRole('button', { name: 'Remove Dana Whitfield' }).click();
  const confirm = page.getByRole('alertdialog');
  await expect(confirm).toContainText('can no longer sign in, and is signed out everywhere');
  await confirm.getByRole('button', { name: 'Cancel' }).click();
  await expect(confirm).toHaveCount(0);
  expect(await actors()).toContain('dana');

  await page.getByRole('button', { name: 'Remove Dana Whitfield' }).click();
  await page.getByRole('alertdialog').getByRole('button', { name: 'Remove', exact: true }).click();
  await expect(page.getByText('dana@example.com', { exact: true })).toHaveCount(0);
  expect(await actors()).not.toContain('dana');
});

test('renaming the company changes the shell at once', async ({ page }) => {
  try {
    await page.goto('/');
    await page.getByRole('button', { name: 'Default, company menu', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Settings' }).click();
    await page.getByRole('link', { name: 'Company', exact: true }).click();
    await expect(page).toHaveURL(/\/settings\/company$/);

    const field = page.getByLabel('Company name');
    const save = page.getByRole('button', { name: 'Save' });
    await expect(field).toHaveValue('Default');
    await expect(save).toBeDisabled();
    await field.fill('   ');
    await expect(page.getByText("The name can't be empty")).toBeVisible();
    await expect(save).toBeDisabled();
    await field.fill('x'.repeat(101));
    await expect(page.getByText('The name can be at most 100 characters')).toBeVisible();
    await expect(save).toBeDisabled();

    await field.fill('  Juniper Holdings ');
    await save.click();
    await expect(page.getByRole('region', { name: 'Notifications' }).getByText('Renamed to Juniper Holdings')).toBeVisible();
    await expect(field).toHaveValue('Juniper Holdings');
    await expect(save).toBeDisabled();
    // The shell shows it straight away, without a reload.
    await page.getByRole('link', { name: 'Back to app' }).click();
    await expect(page.getByRole('button', { name: 'Juniper Holdings, company menu', exact: true })).toBeVisible();
    expect((await agent<{ organization: { name: string } }>('org.get', {})).organization.name).toBe('Juniper Holdings');
    await expect(agent('org.update', { name: 'Agent Co' })).rejects.toThrow(/403/);

    await page.reload();
    await expect(page.getByRole('button', { name: 'Juniper Holdings, company menu', exact: true })).toBeVisible();
    await expect(page).toHaveURL('/');
  } finally {
    const { name } = (await agent<{ organization: { name: string } }>('org.get', {})).organization;
    if (name !== 'Default') await personCommand(page, 'org.update', { name: 'Default' });
  }
});

test('people and company settings fit a phone', async ({ page }, testInfo) => {
  await personCommand(page, 'person.add', { email: 'mobile.tester+ap@example.com', name: 'Mobile Tester With A Long Name' });
  try {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto('/settings/people');
    const remove = page.getByRole('button', { name: 'Remove Mobile Tester With A Long Name' });
    await expect(remove).toBeVisible();
    await expect(page.getByText('mobile.tester+ap@example.com', { exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath('people-mobile.png'), fullPage: true });

    await page.getByRole('button', { name: 'Open navigation' }).click();
    await page.getByRole('link', { name: 'Company', exact: true }).click();
    await expect(page.getByLabel('Company name')).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath('company-mobile.png'), fullPage: true });
  } finally {
    if ((await actors()).includes('mobile.tester')) await personCommand(page, 'person.remove', { actor: 'mobile.tester' });
  }
});
