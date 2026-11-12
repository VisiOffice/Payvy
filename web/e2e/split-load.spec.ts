// "Create N bills" at real-world size: a 40-page file of 38 invoices, the
// first running three pages, three of them copies of bills already here and
// one a scan the model can't read. One confirm (double-clicked) makes one
// batch; the progress counts only go up and always add up; a reload in the
// middle picks the batch up where it is; a retry of the same request gets the
// same batch back; and every part ends somewhere, none left "Reading…".
import fs from 'node:fs';
import { expect, test, type Page } from '@playwright/test';
import { agent, state } from './api';
import { CHOLLA_UNREADABLE_PAGE } from './fake-model';
import { pdfPages } from './pdf';

const VENDOR = 'Cholla Freight Lines';
const STACKED = [5, 17, 29];
const cholla = (n: number) => `CF-${1000 + n}`;
const cents = (n: number) => (100 + n) * 100;

interface Counts { reading: number; created: number; stacked: number; needs_you: number; withdrawn: number }
interface Batch { id: string; done: boolean; counts: Counts; parts: { state: string; bill_id: string | null; note: string | null }[] }

const region = (page: Page) => page.getByRole('region', { name: 'Bills from this file' });

test('38 invoices in 40 pages: one batch, honest progress, resumes after a reload', async ({ page }, info) => {
  test.setTimeout(120_000);
  const vendor = (await agent<{ id: string }>('vendor.create', { name: VENDOR })).id;
  for (const n of STACKED) {
    await agent('bill.create', { vendor_id: vendor, invoice_number: cholla(n), invoice_date: '2026-11-05', amount_cents: cents(n) });
  }

  const tag = `split-load-${Date.now()}`;
  const pages: string[][] = [
    [VENDOR, `Invoice ${cholla(1)}`, 'Continued on next page', tag],
    [VENDOR, `Invoice ${cholla(1)} continued`, tag],
    [VENDOR, `Invoice ${cholla(1)} total $101.00`, tag],
  ];
  for (let n = 2; n <= 38; n++) {
    pages.push(n + 2 === CHOLLA_UNREADABLE_PAGE ? [VENDOR, '(smudged)', tag] : [VENDOR, `Invoice ${cholla(n)} total $${100 + n}.00`, tag]);
  }
  expect(pages).toHaveLength(40);
  const doc = await agent<{ id: string }>('document.upload', { filename: `${tag}.pdf`, content_base64: pdfPages(pages).toString('base64') });

  await page.goto(`/inbox/${doc.id}`);
  await page.getByRole('button', { name: 'Read it' }).click();
  await expect(page.locator('.payvy-split-explain')).toHaveText('40 pages → 38 invoices · CF-1001 runs 3 pages · nothing left over');

  // Every Create the page sends, and what the server answered.
  const sent: string[] = [];
  page.on('request', (r) => { if (r.url().endsWith('/api/commands/document.create_bills')) sent.push(r.postData() ?? ''); });

  await page.getByRole('button', { name: 'Create 38 bills' }).click();
  const dialog = page.getByRole('dialog');
  const started = Date.now();
  await dialog.getByRole('button', { name: 'Create 38 bills' }).dblclick();

  // Follow the progress line until done, reloading once part way through.
  const seen: { at: number; finished: number; text: string }[] = [];
  let reloaded = false;
  for (;;) {
    const text = (await region(page).locator('header').textContent({ timeout: 15_000 })) ?? '';
    if (text.startsWith('Bills from this file')) break;
    const m = /(\d+) of 38 done · (.*)\. You can leave/.exec(text);
    expect(m, text).not.toBeNull();
    const finished = Number(m![1]);
    // The parts named in the summary add up to 38, and to "N of 38 done".
    const named = [...m![2]!.matchAll(/(\d+) (?:bills? created|stacked|needs? you|handled|still reading)/g)].map((x) => Number(x[1]));
    expect(named.reduce((a, b) => a + b, 0), text).toBe(38);
    const reading = Number(/(\d+) still reading/.exec(m![2]!)?.[1] ?? 0);
    expect(38 - reading, text).toBe(finished);
    if (seen.length) expect(finished, text).toBeGreaterThanOrEqual(seen.at(-1)!.finished);
    seen.push({ at: Date.now() - started, finished, text });
    if (!reloaded && finished >= 10) {
      reloaded = true;
      await page.reload();
      // Back on the same batch, not a fresh Create button.
      await expect(page.getByRole('button', { name: 'Create 38 bills' })).toHaveCount(0);
      continue;
    }
    expect(Date.now() - started, 'the batch took too long').toBeLessThan(90_000);
    await page.waitForTimeout(250);
  }
  const total = Date.now() - started;
  await expect(region(page).locator('header')).toContainText('34 bills created · 3 stacked on existing bills · 1 needs you');

  // The double click sent one request (or two of the same); a retry of it
  // gets the batch it made, marked repeated, and splits nothing more.
  expect(sent.length).toBeGreaterThanOrEqual(1);
  expect(new Set(sent.map((s) => JSON.parse(s).input.request_id)).size).toBe(1);
  const retry = await page.request.post('/api/commands/document.create_bills', {
    headers: { 'x-payvy-client': 'web', 'content-type': 'application/json' },
    data: sent[0],
  });
  const repeated = (await retry.json()).result as Batch & { repeated?: boolean };
  expect(retry.ok()).toBe(true);
  expect(repeated.repeated).toBe(true);

  const detail = await agent<{ bill_batch: Batch }>('document.get', { document_id: doc.id });
  const batch = detail.bill_batch;
  expect(repeated.id).toBe(batch.id);
  expect(batch.done).toBe(true);
  expect(batch.parts).toHaveLength(38);
  expect(batch.counts).toEqual({ reading: 0, created: 34, stacked: 3, needs_you: 1, withdrawn: 0 });
  await expect(page.getByRole('list', { name: 'Split into' }).getByRole('listitem')).toHaveCount(38);

  // 37 bills for the vendor: 34 new and the 3 that were here, each copy
  // stacked once.
  const { bills } = await agent<{ bills: { bill: { invoice_number: string }; received_count: number }[] }>('bill.list', { vendor_id: vendor, limit: 100 });
  expect(bills).toHaveLength(37);
  for (const b of bills) {
    expect(b.received_count, b.bill.invoice_number).toBe(STACKED.map(cholla).includes(b.bill.invoice_number) ? 2 : 1);
  }
  const unread = batch.parts.find((p) => p.state === 'needs_you')!;
  expect(unread.note).toMatch(/^Needs you: it couldn't be read/);

  await region(page).getByText('Each invoice').click();
  await expect(region(page).getByRole('list', { name: 'Results' }).getByRole('listitem')).toHaveCount(38);
  await expect(region(page).getByText('Reading…')).toHaveCount(0);
  await page.screenshot({ path: info.outputPath('split-load-done.png'), fullPage: true, animations: 'disabled' });

  // The worker said nothing went wrong.
  const log = fs.readFileSync(state().serverLog, 'utf8');
  expect(log).not.toMatch(/bill batches: /);

  const report = { total_ms: total, requests: sent.length, samples: seen.length, reloaded, progress: seen.map((s) => `${s.at}ms ${s.finished}/38`) };
  console.log(JSON.stringify(report));
  fs.writeFileSync(info.outputPath('split-load.json'), JSON.stringify({ ...report, seen }, null, 2));
});
