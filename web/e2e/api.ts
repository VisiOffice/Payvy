// Setting up data the way an agent would: through the command API with an
// agent key. What a person must do (accept, approve, release) the tests do
// in the browser.
import fs from 'node:fs';
import path from 'node:path';
import { BASE_URL, ROOT, STATE_FILE, type State } from './env';

export const state = (): State => JSON.parse(fs.readFileSync(STATE_FILE, 'utf8'));

export async function agent<R = Record<string, unknown>>(command: string, input: object): Promise<R> {
  const res = await fetch(`${BASE_URL}/api/commands/${command}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${state().agentKey}` },
    body: JSON.stringify({ input, mode: 'commit' }),
  });
  const body = await res.json();
  if (!res.ok) throw new Error(`${command}: ${res.status} ${JSON.stringify(body)}`);
  return body.result as R;
}

export async function vendor(name: string): Promise<string> {
  return (await agent<{ id: string }>('vendor.create', { name })).id;
}

/** Uploads a fixture invoice and has the (fake) model read it. */
export async function inboxDocument(fixture: string): Promise<string> {
  const file = fs.readFileSync(path.join(ROOT, 'fixtures/invoices', fixture));
  const doc = await agent<{ id: string }>('document.upload', {
    filename: fixture,
    content_base64: file.toString('base64'),
  });
  await agent('document.extract', { document_id: doc.id });
  return doc.id;
}

export async function bill(input: {
  vendor_id: string;
  invoice_number: string;
  invoice_date: string;
  amount_cents: number;
}): Promise<string> {
  return (await agent<{ bill: { id: string } }>('bill.create', input)).bill.id;
}
