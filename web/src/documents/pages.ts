// A document's pages as document.get returns them, and their history in words.
import type { PageRef } from '../viewer/DocumentViewer';

export type { PageRef };

/** One version of the arrangement. Never edited; each change adds one. */
export interface ArrangementVersion {
  id: string;
  version: number;
  pages: PageRef[];
  created_at: string;
  created_by: string;
}

export interface Pages {
  /** Pages in the original; null when the file can't be read as a PDF. */
  count: number | null;
  /** 0 while the pages were never arranged. */
  version: number;
  arrangement_id: string | null;
  /** In the order shown; null when the page count isn't known. */
  arrangement: PageRef[] | null;
  locked: boolean;
  locked_reason: string | null;
  /** Newest first. */
  versions: ArrangementVersion[];
}

/** One difference between two arrangements, with the original's page numbers. */
export type Change =
  | { change: 'moved'; page: number; to: number; of: number }
  | { change: 'reordered'; order: number[] }
  | { change: 'rotated'; page: number; from: number; to: number }
  | { change: 'hidden'; page: number }
  | { change: 'restored'; page: number };

export interface DocumentEvent {
  id: number;
  kind: string;
  actor: string;
  at: string;
  payload: { changes?: Change[]; summary?: string } & Record<string, unknown>;
}

function words(c: Change): string {
  switch (c.change) {
    case 'moved':
      return `moved page ${c.page} to ${c.to === 1 ? 'first' : c.to === c.of ? 'last' : `position ${c.to}`}`;
    case 'reordered':
      return `put the pages in the order ${c.order.join(', ')}`;
    case 'rotated':
      return c.to === 0 ? `turned page ${c.page} upright` : `rotated page ${c.page} to ${c.to}°`;
    case 'hidden':
      return `hid page ${c.page}`;
    case 'restored':
      return `restored page ${c.page}`;
  }
}

/** "moved page 3 to first and hid page 4" */
export function describe(changes: Change[]): string {
  const parts = changes.map(words);
  if (parts.length <= 1) return parts[0] ?? '';
  return `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`;
}

/** "page 3", "pages 5–6", "pages 1–3 and 7", as the server words them. */
export function describePages(pages: number[]): string {
  const runs: [number, number][] = [];
  for (const p of pages) {
    const last = runs[runs.length - 1];
    if (last && p === last[1] + 1) last[1] = p;
    else runs.push([p, p]);
  }
  const parts = runs.map(([a, b]) => (a === b ? `${a}` : `${a}–${b}`));
  const list = parts.length <= 1 ? (parts[0] ?? '') : `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`;
  return `${pages.length === 1 ? 'page' : 'pages'} ${list}`;
}

/** Events that change which pages a document has, or how they're shown. */
export const PAGE_EVENTS = ['document.arranged', 'document.split', 'document.split_from', 'document.unsplit'];

/** Who did it and what, for the page history: "robert moved page 3 to first". */
export function historyLine(e: DocumentEvent, nameOf: (actor: string) => string = (a) => a): string {
  const what = e.payload.changes ? describe(e.payload.changes) : (e.payload.summary ?? 'rearranged the pages');
  return `${nameOf(e.actor)} ${what}`;
}

/** The arrangement with one page moved from one place to another. */
export function move(pages: PageRef[], from: number, to: number): PageRef[] {
  const next = [...pages];
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved!);
  return next;
}

/** The arrangement with one page changed. */
export function update(pages: PageRef[], index: number, patch: Partial<PageRef>): PageRef[] {
  return pages.map((p, i) => (i === index ? { ...p, ...patch } : p));
}
