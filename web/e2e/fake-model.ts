// A stand-in for an OpenAI-compatible model, so extraction runs through the
// server's real path (request, checks, vendor match, suggested bill) without
// a key or a bill. It answers with what each fixture invoice prints.
// A file from "American Reclamation" holds two invoices and a statement
// page; the fake finds them, and reads only the pages it is asked to.
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
  [
    // Generated in bills.spec.ts: a vendor's second, genuinely separate bill
    // that happens to reuse an invoice number.
    'SG-4120',
    {
      ...blank,
      kind: 'invoice',
      vendor_name: 'Saguaro Glass & Mirror',
      invoice_number: 'SG-4120',
      invoice_date: '2026-11-04',
      currency: 'USD',
      total: '512.00',
    },
  ],
  [
    'SR-7781',
    {
      ...blank,
      kind: 'invoice',
      vendor_name: 'Sonoran Pest Control',
      invoice_number: 'SR-7781',
      invoice_date: '2026-11-03',
      currency: 'USD',
      total: '185.00',
    },
  ],
];

const reclamation = (invoice_number: string, total: string, invoices: object[] = []): Read => ({
  ...blank,
  kind: 'invoice',
  vendor_name: 'American Reclamation',
  invoice_number,
  invoice_date: '2026-11-02',
  currency: 'USD',
  total,
  invoices,
});

/**
 * The 22-page bill.com case, cut down: invoice 85273 on pages 1–2 ("Continued
 * on next page"), 85274 on page 3, and the account statement on page 4.
 * Told to read only some pages, it reads the invoice on them.
 */
function readReclamation(pages: number[] | null): Read {
  if (!pages) {
    return reclamation('85273', '8,034.89', [
      { pages: [1, 2], invoice_number: '85273', total: '8,034.89' },
      { pages: [3], invoice_number: '85274', total: '1,200.00' },
    ]);
  }
  if (pages.includes(1) || pages.includes(2)) return reclamation('85273', '8,034.89');
  if (pages.includes(3)) return reclamation('85274', '1,200.00');
  return { ...blank, kind: 'statement', vendor_name: 'American Reclamation' };
}

const saguaro = (invoice_number: string, total: string, invoices: object[] = []): Read => ({
  ...blank,
  kind: 'invoice',
  vendor_name: 'Saguaro Storage Co',
  invoice_number,
  invoice_date: '2026-11-04',
  currency: 'USD',
  total,
  lines: [{ description: 'Storage container rental', amount: total }],
  invoices,
});

/**
 * Saguaro Storage: invoice SS-101 runs pages 1–3, SS-102 is page 4, SS-103
 * page 5, and page 6 is a remittance slip that belongs to no bill.
 */
function readSaguaro(pages: number[] | null): Read {
  if (!pages) {
    return saguaro('SS-101', '300.00', [
      { pages: [1, 2, 3], invoice_number: 'SS-101', total: '300.00' },
      { pages: [4], invoice_number: 'SS-102', total: '120.00' },
      { pages: [5], invoice_number: 'SS-103', total: '45.50' },
    ]);
  }
  if (pages.some((p) => p <= 3)) return saguaro('SS-101', '300.00');
  if (pages.includes(4)) return saguaro('SS-102', '120.00');
  if (pages.includes(5)) return saguaro('SS-103', '45.50');
  return { ...blank, kind: 'other', vendor_name: 'Saguaro Storage Co' };
}

/**
 * Cholla Freight Lines, at real-world size (split-load.spec.ts): 40 pages,
 * 38 invoices. CF-1001 runs pages 1–3; CF-1002 to CF-1038 are one page each
 * (CF-n on page n + 2). Page 32 is a smudged scan: the reader finds an
 * invoice there but can't read it, and answers with something that isn't
 * the schema when asked to.
 */
export const CHOLLA_UNREADABLE_PAGE = 32;
const cholla = (n: number) => `CF-${1000 + n}`;
const chollaTotal = (n: number) => `${100 + n}.00`;
const chollaRead = (n: number, invoices: object[] = []): Read => ({
  ...blank,
  kind: 'invoice',
  vendor_name: 'Cholla Freight Lines',
  invoice_number: cholla(n),
  invoice_date: '2026-11-05',
  currency: 'USD',
  total: chollaTotal(n),
  lines: [{ description: 'Freight', amount: chollaTotal(n) }],
  invoices,
});
/** null: the answer isn't the schema. */
function readCholla(pages: number[] | null): Read | null {
  if (!pages) {
    const invoices: object[] = [{ pages: [1, 2, 3], invoice_number: cholla(1), total: chollaTotal(1) }];
    for (let n = 2; n <= 38; n++) {
      const page = n + 2;
      invoices.push(
        page === CHOLLA_UNREADABLE_PAGE
          ? { pages: [page], invoice_number: null, total: null }
          : { pages: [page], invoice_number: cholla(n), total: chollaTotal(n) },
      );
    }
    return chollaRead(1, invoices);
  }
  if (pages.includes(CHOLLA_UNREADABLE_PAGE)) return null;
  if (pages.some((p) => p <= 3)) return chollaRead(1);
  return chollaRead(pages[0]! - 2);
}

/** The pages the server asked for ("Read only pages 1, 2 of this file"), or null for all. */
function pagesAsked(body: { messages?: { content?: unknown }[] }): number[] | null {
  for (const m of body.messages ?? []) {
    if (!Array.isArray(m.content)) continue;
    for (const part of m.content as { text?: string }[]) {
      const asked = part.text?.match(/Read only pages? ([\d, ]+) of this file/);
      if (asked) return asked[1]!.split(',').map((n) => Number(n.trim()));
    }
  }
  return null;
}

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
      const body = JSON.parse(raw);
      const file = fileOf(body);
      if (file.includes('Cholla Freight')) {
        const read = readCholla(pagesAsked(body));
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(
          JSON.stringify({
            model: 'fake',
            choices: [{ finish_reason: 'stop', message: { content: read ? JSON.stringify(read) : 'Sorry, the scan is too smudged to read.' } }],
          }),
        );
        return;
      }
      let read = file.includes('American Reclamation')
        ? readReclamation(pagesAsked(body))
        : file.includes('Saguaro Storage')
        ? readSaguaro(pagesAsked(body))
        : (READS.find(([needle]) => file.includes(needle))?.[1] ?? blank);
      if (file.includes('V2-UI')) {
        read = {...blank, kind:'invoice',vendor_name:'Richer review vendor',invoice_number:'V2-UI',invoice_date:'2026-11-11',currency:'USD',total:'30.86',
          lines:[{description:'Disposal',amount:'30.86',details:{quantity:'2.5',unit:'ton',unit_price:'12.345'}}]};
        const rows=read.lines as {description:string;amount:string;details:Record<string,unknown>}[];
        for(const row of rows){
          const states:Record<string,string>={};const evidence:object[]=[];
          for(const field of ['description','amount','quantity','unit','unit_price','tax_amount','tax_rate','item_code','service_start','service_end']) {
            const present=['description','amount'].includes(field) || row.details[field] != null;
            states[field]=present?'present':'absent';
            if(present)evidence.push({field,page:1,quote:'Disposal 2.5 ton 12.345 30.86'});
          }
          row.details={...row.details,states,evidence};
        }
        read.coverage={pages_read:[1],complete:true,reason:null};
      }
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
