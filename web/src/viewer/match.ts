// Finding what was extracted on the page. Payvy keeps every value as printed
// (amounts, invoice numbers) or as an ISO date, so for a PDF with real text
// the value can be found in the page's own text: no coordinates from the model.

export interface Item {
  str: string;
  /** Box of the whole item in page pixels. */
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];

/** The ways an ISO date is commonly printed on US invoices. */
export function dateForms(iso: string): string[] {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso);
  if (!m) return [iso];
  const [, y, mm, dd] = m as unknown as [string, string, string, string];
  const mon = MONTHS[Number(mm) - 1]!;
  const d = String(Number(dd));
  const mo = String(Number(mm));
  return [
    `${mm}/${dd}/${y}`, `${mo}/${d}/${y}`, `${mm}/${dd}/${y.slice(2)}`, `${mo}/${d}/${y.slice(2)}`,
    `${mon} ${d}, ${y}`, `${mon.slice(0, 3)} ${d}, ${y}`, `${mon.slice(0, 3)}. ${d}, ${y}`,
    `${d} ${mon} ${y}`, `${d} ${mon.slice(0, 3)} ${y}`, `${mm}-${dd}-${y}`, iso,
  ];
}

const norm = (s: string) => s.toLowerCase().replace(/\s+/g, ' ');

/**
 * Where any of `candidates` appears in the page's text items, as rectangles
 * (one per item the match spans, trimmed by character share). The first
 * candidate that matches wins; all its matches are returned.
 */
export function find(items: Item[], candidates: string[]): Rect[] {
  // The page as one string, with each character's item and offset.
  let text = '';
  const owner: { item: number; offset: number }[] = [];
  items.forEach((it, i) => {
    const s = norm(it.str);
    for (let k = 0; k < s.length; k++) {
      text += s[k];
      owner.push({ item: i, offset: k });
    }
    if (!text.endsWith(' ')) {
      text += ' ';
      owner.push({ item: -1, offset: 0 });
    }
  });

  for (const c of candidates) {
    const needle = norm(c.trim());
    if (needle.length < 2) continue;
    const rects: Rect[] = [];
    let from = 0;
    for (;;) {
      const at = text.indexOf(needle, from);
      if (at < 0) break;
      // Not part of a longer number or word: "100.00" mustn't match "1,100.00".
      const before = text[at - 1];
      const after = text[at + needle.length];
      const edge = (ch: string | undefined) => ch === undefined || !/[a-z0-9]/.test(ch);
      if (edge(before) && edge(after)) rects.push(...span(items, owner, at, at + needle.length));
      from = at + needle.length;
    }
    if (rects.length) return rects;
  }
  return [];
}

function span(
  items: Item[],
  owner: { item: number; offset: number }[],
  start: number,
  end: number,
): Rect[] {
  const byItem = new Map<number, { first: number; last: number }>();
  for (let k = start; k < end; k++) {
    const o = owner[k]!;
    if (o.item < 0) continue;
    const r = byItem.get(o.item);
    if (r) r.last = o.offset;
    else byItem.set(o.item, { first: o.offset, last: o.offset });
  }
  return [...byItem].map(([i, { first, last }]) => {
    const it = items[i]!;
    const n = Math.max(norm(it.str).length, 1);
    return { x: it.x + (it.w * first) / n, y: it.y, w: (it.w * (last - first + 1)) / n, h: it.h };
  });
}

const pad2 = (n: number) => String(n).padStart(2, '0');

/** A real calendar date as YYYY-MM-DD, or null. */
function isoOf(y: number, m: number, d: number): string | null {
  if (m < 1 || m > 12 || d < 1 || d > 31) return null;
  const t = new Date(Date.UTC(y, m - 1, d));
  if (t.getUTCMonth() !== m - 1) return null;
  return `${y}-${pad2(m)}-${pad2(d)}`;
}

/** Two-digit years are this century: "26" is 2026. */
const fullYear = (y: string) => (y.length === 2 ? 2000 + Number(y) : Number(y));

/** "June", "Jun" or "Jun." → 6; null if it isn't a month. */
function monthOf(s: string): number | null {
  const t = s.replace(/\.$/, '').toLowerCase();
  if (t.length < 3) return null;
  const i = MONTHS.findIndex((m) => m.toLowerCase().startsWith(t));
  return i < 0 ? null : i + 1;
}

/**
 * A date as printed on a US invoice, as YYYY-MM-DD: the reverse of
 * `dateForms`. Numbers are month first ("6/15/26", "06-15-2026"); a month
 * may be named ("June 15, 2026", "Jun. 15 2026", "15 June 2026"). Null when
 * the text isn't one date.
 */
export function parseDate(text: string): string | null {
  const s = text.trim().replace(/\s+/g, ' ').replace(/[.,;]$/, '');
  let m = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(s);
  if (m) return isoOf(Number(m[1]), Number(m[2]), Number(m[3]));
  m = /^(\d{1,2})[/.-](\d{1,2})[/.-](\d{2}|\d{4})$/.exec(s);
  if (m) return isoOf(fullYear(m[3]!), Number(m[1]), Number(m[2]));
  m = /^([A-Za-z]{3,9}\.?) (\d{1,2})(?:st|nd|rd|th)?,? (\d{4})$/.exec(s);
  if (m) {
    const mo = monthOf(m[1]!);
    return mo ? isoOf(Number(m[3]), mo, Number(m[2])) : null;
  }
  m = /^(\d{1,2}) ([A-Za-z]{3,9}\.?),? (\d{4})$/.exec(s);
  if (m) {
    const mo = monthOf(m[2]!);
    return mo ? isoOf(Number(m[3]), mo, Number(m[1])) : null;
  }
  return null;
}

/**
 * An amount as printed, as it is typed into an amount field: "$8,034.89" →
 * "8034.89", "USD 1,200" → "1200.00". Null when the text isn't one amount.
 */
export function parseAmount(text: string): string | null {
  const s = text.trim().replace(/^(?:USD|US\$|\$)\s*/i, '').replace(/\s*(?:USD)$/i, '');
  const m = /^(\d{1,3}(?:,\d{3})+|\d+)(?:\.(\d{1,2}))?$/.exec(s);
  if (!m) return null;
  return `${m[1]!.replaceAll(',', '')}.${(m[2] ?? '').padEnd(2, '0')}`;
}
