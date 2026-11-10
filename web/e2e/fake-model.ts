// A stand-in for an OpenAI-compatible model, so extraction runs through the
// server's real path (request, checks, vendor match, suggested bill) without
// a key or a bill. It answers with what each fixture invoice prints.
import http from 'node:http';

type Read = Record<string, unknown>;

const blank: Read = {
  kind: 'other',
  vendor_name: null,
  vendor_email: null,
  vendor_address: null,
  account_number: null,
  invoice_number: null,
  invoice_date: null,
  due_date: null,
  terms: null,
  currency: null,
  po_number: null,
  subtotal: null,
  tax: null,
  total: null,
  lines: [],
  notes: null,
};

/** Keyed by text found in the file itself. */
const READS: [string, Read][] = [
  [
    'DP-5521',
    {
      ...blank,
      kind: 'invoice',
      vendor_name: 'Desert Plumbing & Drain',
      vendor_address: '880 W Broadway, Tempe, AZ',
      invoice_number: 'DP-5521',
      invoice_date: '2026-10-28',
      due_date: '2026-11-12',
      currency: 'USD',
      po_number: 'QR-2231',
      total: '1,420.00',
      lines: [
        { description: 'Service call', amount: '95.00' },
        { description: 'Replace 40 gal water heater', amount: '1,280.00' },
        { description: 'Haul away old unit', amount: '45.00' },
      ],
    },
  ],
  [
    '30117',
    {
      ...blank,
      kind: 'invoice',
      vendor_name: 'Mesa Electric Supply Co.',
      vendor_address: '3301 E Main St, Mesa, AZ',
      invoice_number: '30117',
      invoice_date: '2026-10-30',
      terms: 'Due on receipt',
      currency: 'USD',
      subtotal: '315.45',
      tax: '26.18',
      total: '341.63',
      lines: [
        { description: '12/2 Romex, 250 ft (2)', amount: '238.00' },
        { description: '20A breakers (10)', amount: '112.50' },
        { description: 'Contractor discount 10%', amount: '-35.05' },
      ],
    },
  ],
  [
    '189727',
    {
      ...blank,
      kind: 'invoice',
      vendor_name: 'King Environmental, LLC',
      vendor_email: 'ar@kingenv.example',
      vendor_address: '12 Quarry Rd, Mesa, AZ 85201',
      account_number: '004417',
      invoice_number: '189727',
      invoice_date: '2026-11-01',
      due_date: '2026-12-01',
      terms: 'Net 30',
      currency: 'USD',
      subtotal: '425.00',
      tax: '34.00',
      total: '$459.00',
      lines: [
        { description: '30 yd roll-off haul, 1420 Elm St', amount: '300.00' },
        { description: 'Disposal, 2.5 tons', amount: '125.00' },
      ],
    },
  ],
  ['ACME', { ...blank, kind: 'statement', vendor_name: 'Acme Supply' }],
];

/** The file's bytes, from the data URL the server sends. */
function fileOf(body: { messages?: { content?: unknown }[] }): string {
  for (const m of body.messages ?? []) {
    if (!Array.isArray(m.content)) continue;
    for (const part of m.content as Record<string, Record<string, string>>[]) {
      const url = part.file?.file_data ?? part.image_url?.url;
      if (url) return Buffer.from(url.slice(url.indexOf(',') + 1), 'base64').toString('latin1');
    }
  }
  return '';
}

export function startFakeModel(port: number): Promise<http.Server> {
  const server = http.createServer((req, res) => {
    let raw = '';
    req.on('data', (c) => (raw += c));
    req.on('end', () => {
      if (req.method !== 'POST' || !req.url?.endsWith('/chat/completions')) {
        res.writeHead(404).end();
        return;
      }
      const file = fileOf(JSON.parse(raw));
      const read = READS.find(([needle]) => file.includes(needle))?.[1] ?? blank;
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          model: 'fake',
          choices: [{ finish_reason: 'stop', message: { content: JSON.stringify(read) } }],
        }),
      );
    });
  });
  return new Promise((resolve) => server.listen(port, '127.0.0.1', () => resolve(server)));
}
