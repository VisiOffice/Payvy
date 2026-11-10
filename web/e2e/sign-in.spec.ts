import { expect, test } from '@playwright/test';
import { state } from './api';
import { PERSON } from './env';
import { linkAfter, logMark } from './mail';

// Signed out: these tests start without the saved cookie.
test.use({ storageState: { cookies: [], origins: [] } });

test('a person signs in with an emailed link and signs out', async ({ page }) => {
  const log = state().serverLog;
  await page.goto('/bills');
  await expect(page).toHaveURL(/\/sign-in$/);

  const mark = logMark(log);
  await page.getByLabel('Work email').fill(PERSON.email);
  await page.getByRole('button', { name: 'Email me a link' }).click();
  await expect(page.getByText(`Check ${PERSON.email}`)).toBeVisible();

  await page.goto(await linkAfter(log, mark));
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  // The token is gone from the address bar once used.
  expect(page.url()).not.toContain('token=');

  await page.reload();
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();

  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(page).toHaveURL(/\/sign-in$/);
  await page.goto('/');
  await expect(page).toHaveURL(/\/sign-in$/);
});

test('a link works once', async ({ page }) => {
  const log = state().serverLog;
  const mark = logMark(log);
  await page.goto('/sign-in');
  await page.getByLabel('Work email').fill(PERSON.email);
  await page.getByRole('button', { name: 'Email me a link' }).click();
  const link = await linkAfter(log, mark);

  await page.goto(link);
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  await page.context().clearCookies();
  await page.goto(link);
  await expect(page.getByText(/expired or was already used/)).toBeVisible();
});

test('an address that cannot sign in gets the same answer and no link', async ({ page }) => {
  const log = state().serverLog;
  const mark = logMark(log);
  await page.goto('/sign-in');
  await page.getByLabel('Work email').fill('mallory@example.com');
  await page.getByRole('button', { name: 'Email me a link' }).click();
  await expect(page.getByText('Check mallory@example.com')).toBeVisible();
  await expect(linkAfter(log, mark, 1_000)).rejects.toThrow();
});
