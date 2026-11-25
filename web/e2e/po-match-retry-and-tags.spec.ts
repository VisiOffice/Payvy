// Phase 1 PO matching: an unmatched bill explains why, a person matches it by
// hand and tags the printed service address by clicking the text on the
// invoice, and the vendor page lists the learned example.
import { expect, test, type Page } from '@playwright/test';
import { agent, vendor } from './api';
import { pdfPages } from './pdf';

async function personCommand<T = unknown>(page: Page, command: string, input: object): Promise<T> {
  const res = await page.request.post(`/api/commands/${command}`, {
    headers: { 'x-payvy-client': 'web' },
    data: { mode: 'commit', input },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const body = (await res.json()) as { result: T };
  return body.result;
}

test('unmatched bill shows why, person matches and tags address by clicking text, vendor lists example', async ({ page }) => {
  await personCommand(page, 'po.configure', { enabled: true });
  const vendorId = await vendor('Tagging Vendor');
  const meRes = await page.request.get('/auth/session', { headers: { 'x-payvy-client': 'web' } });
  expect(meRes.ok(), await meRes.text()).toBeTruthy();
  const ownerActor = ((await meRes.json()) as { person: { actor: string } }).person.actor;

  const po = await personCommand<{ id: string }>(page, 'po.create', {
    vendor_id: vendorId,
    po_number: 'PO-TAG-E2E',
    owner: ownerActor,
    authorized_cents: 100_000,
    service_address: '120 Main St, Austin, TX',
  });
  await personCommand(page, 'po.activate', { po_id: po.id });

  const doc = await agent<{ id: string }>('document.upload', {
    filename: `tag-invoice-${Date.now()}.pdf`,
    content_base64: pdfPages([
      ['TAGGING VENDOR', 'Invoice #: TAG-100', 'Total: $450.00', '120 Main Street'],
    ]).toString('base64'),
  });

  const today = new Date().toISOString().slice(0, 10);
  const created = await agent<{ bill: { id: string } }>('bill.create', {
    vendor_id: vendorId,
    invoice_number: 'TAG-100',
    invoice_date: today,
    amount_cents: 45_000,
    document_id: doc.id,
  });
  const billId = created.bill.id;

  // Bill page shows why it did not match and offers Try again.
  await page.goto(`/bills/${billId}`);
  await expect(
    page.getByText(/No PO number or service address was read from this invoice\. This vendor has 1 open purchase order\./),
  ).toBeVisible();
  await expect(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();

  // Reserve against PO-TAG-E2E, then tag the service address by clicking text on the PDF.
  await page.getByRole('button', { name: 'Reserve', exact: true }).click();
  await expect(page.getByText('What on the invoice ties it to PO-TAG-E2E?')).toBeVisible();

  const addrInput = page.getByRole('textbox', { name: 'Service address printed on invoice' });
  await addrInput.click();
  const addrRun = page.locator('.payvy-viewer-canvas .payvy-copy-run[data-run="120 Main Street"]');
  await expect(addrRun).toBeVisible();
  await addrRun.click();
  await expect(addrInput).toHaveValue('120 Main Street');

  await page.getByRole('button', { name: 'Save example', exact: true }).click();
  await expect(page.getByText('What on the invoice ties it to PO-TAG-E2E?')).toHaveCount(0);

  // Vendor page lists the learned example with Retire…
  await page.goto(`/vendors/${vendorId}`);
  const poSection = page.locator('#purchase-orders');
  await expect(poSection.getByText('Learned PO matching examples')).toBeVisible();
  await expect(poSection.getByText('“120 Main Street”')).toBeVisible();
  await expect(poSection.getByRole('button', { name: 'Retire…', exact: true })).toBeVisible();
});
