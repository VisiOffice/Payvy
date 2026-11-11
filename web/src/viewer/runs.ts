// Text on a PDF page, grouped into what a person would copy: a run of words
// on one line with ordinary spaces between them ("AMERICAN RECLAMATION INC",
// "$8,034.89", "6/15/26"). pdf.js gives items of any size, from one glyph to a
// whole line, so items are split into words (placed by their share of the
// item's width) and words are joined again where they sit close together.
import type { Item, Rect } from './match';

export interface Word extends Rect {
  str: string;
}

export interface Run extends Rect {
  str: string;
  words: Word[];
}

/** The words of the page's text items, each with its box. */
export function words(items: Item[]): Word[] {
  const out: Word[] = [];
  for (const it of items) {
    const n = it.str.length;
    if (!n) continue;
    const re = /\S+/g;
    for (let m = re.exec(it.str); m; m = re.exec(it.str)) {
      out.push({
        str: m[0],
        x: it.x + (it.w * m.index) / n,
        y: it.y,
        w: (it.w * m[0].length) / n,
        h: it.h,
      });
    }
  }
  return out;
}

/** Two words share a line when their middles are within half a line of each other. */
const sameLine = (a: Rect, b: Rect) => Math.abs(a.y + a.h / 2 - (b.y + b.h / 2)) < Math.min(a.h, b.h) / 2;

/**
 * Words grouped into runs. A run breaks at a gap wider than about one space
 * (two spaces or a column), and after a label ending in a colon, so
 * "Invoice #: 85273" gives "Invoice #:" and "85273".
 */
export function runs(items: Item[]): Run[] {
  const ws = words(items);
  // Lines: each word joins the first line it sits on.
  const lines: Word[][] = [];
  for (const w of ws) {
    const line = lines.find((l) => sameLine(l[0]!, w));
    if (line) line.push(w);
    else lines.push([w]);
  }
  const out: Run[] = [];
  for (const line of lines) {
    line.sort((a, b) => a.x - b.x);
    let run: Word[] = [];
    const flush = () => {
      if (run.length) out.push(join(run));
      run = [];
    };
    for (const w of line) {
      const prev = run[run.length - 1];
      if (prev) {
        const gap = w.x - (prev.x + prev.w);
        const h = Math.min(prev.h, w.h);
        if (gap > 0.8 * h || prev.str.endsWith(':') || Math.abs(prev.h - w.h) > 0.4 * h) flush();
      }
      run.push(w);
    }
    flush();
  }
  return out;
}

function join(ws: Word[]): Run {
  const x = Math.min(...ws.map((w) => w.x));
  const y = Math.min(...ws.map((w) => w.y));
  const right = Math.max(...ws.map((w) => w.x + w.w));
  const bottom = Math.max(...ws.map((w) => w.y + w.h));
  // Pieces that touch are one word split across items (some PDFs place each
  // glyph on its own); anything wider apart is a space.
  const merged: Word[] = [];
  for (const w of ws) {
    const prev = merged[merged.length - 1];
    if (prev && w.x - (prev.x + prev.w) < 0.12 * Math.min(prev.h, w.h)) {
      const right = Math.max(prev.x + prev.w, w.x + w.w);
      merged[merged.length - 1] = { ...prev, str: prev.str + w.str, w: right - prev.x };
    } else merged.push(w);
  }
  return { str: merged.map((w) => w.str).join(' '), x, y, w: right - x, h: bottom - y, words: merged };
}
