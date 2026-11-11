import { expect, test, type Page } from '@playwright/test';
import { agent, bill, vendor } from './api';

// What only a person may do (turn POs on, set a vendor's PO rule) goes
// through the signed-in session; the rest is set up as an agent would.
async function command(page: Page, name: string, input: object) {
  const r = await page.request.post(`/api/commands/${name}`, { headers: { 'x-payvy-client': 'web' }, data: { mode: 'commit', input } });
  expect(r.ok(), await r.text()).toBe(true);
  return (await r.json()).result;
}

const poNav = (page: Page) => page.locator('a[href="/purchase-orders"]');

test('an admin turns purchase orders on, a bill is matched to one, and turning them off hides it all', async ({ page }, info) => {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Waiting on you', exact: true })).toBeVisible();
  // Off by default: no Purchase orders in the workspace nav.
  await expect(poNav(page)).toHaveCount(0);

  await page.goto('/settings/purchase-orders');
  await expect(page.getByRole('heading', { name: 'Purchase orders', exact: true })).toBeVisible();
  const toggle = page.getByRole('switch', { name: 'Use purchase orders', exact: true });
  await expect(toggle).not.toBeChecked();
  await toggle.click();
  await expect(toggle).toBeChecked();
  await expect(page.getByText(/Last changed by robert/)).toBeVisible();
  await expect(poNav(page)).toHaveCount(1);
  await page.screenshot({ path: info.outputPath('po-settings.png'), fullPage: true });

  let id = '';
  try {
    // A vendor whose bills need a PO, a draft PO an agent made, and a bill.
    const v = await vendor('PO Test Hauler');
    await command(page, 'po.set_vendor_settings', { vendor_id: v, require_po: true });
    await agent('po.create', { vendor_id: v, po_number: 'PO-E2E-1', owner: 'robert', authorized_cents: 100_000 });
    id = await bill({ vendor_id: v, invoice_number: 'PO-E2E-INV', invoice_date: '2026-11-10', amount_cents: 40_000 });

    // A person activates the PO in the app.
    await poNav(page).click();
    await expect(page.getByRole('heading', { name: 'Purchase orders', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'PO-E2E-1', exact: true }).click();
    await page.getByRole('button', { name: 'Activate', exact: true }).click();
    await expect(page.getByText('PO-E2E-1 is active').first()).toBeVisible();

    // The bill can't be paid until it is matched; reserving clears it.
    await page.goto(`/bills/${id}`);
    await expect(page.getByRole('heading', { name: 'Purchase order', exact: true })).toBeVisible();
    await expect(page.getByText("Can't be paid yet: no po")).toBeVisible();
    await page.getByRole('button', { name: 'Reserve', exact: true }).click();
    await expect(page.getByText('Reserved against the purchase order').first()).toBeVisible();
    await expect(page.getByText("Can't be paid yet: no po")).toHaveCount(0);
    await expect(page.getByText(/PO-E2E-1 · \$400\.00 · reserved/)).toBeVisible();
  } finally {
    // Turning them off hides the nav and the bill's PO section.
    await page.goto('/settings/purchase-orders');
    const off = page.getByRole('switch', { name: 'Use purchase orders', exact: true });
    await expect(off).toBeChecked();
    await off.click();
    await expect(off).not.toBeChecked();
  }
  await expect(poNav(page)).toHaveCount(0);
  await page.goto(`/bills/${id}`);
  await expect(page.getByText('PO-E2E-INV').first()).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Purchase order', exact: true })).toHaveCount(0);
});

test('the settings page fits a phone', async ({ page }, info) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/settings/purchase-orders');
  await expect(page.getByRole('switch', { name: 'Use purchase orders', exact: true })).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await page.screenshot({ path: info.outputPath('po-settings-390.png'), fullPage: true });
});
