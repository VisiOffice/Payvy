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
