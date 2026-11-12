// The inbox two ways: a table to scan and sort what the reader found in each
// file, and a board grouped by what each file needs next.
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { Link } from '@astryxdesign/core/Link';
import { Allowed } from '../permissions';
import { date, dueLabel, money } from '../money';
import { CHECK } from './checks';
import type { Doc, SplitFrom } from './types';

/** What a document's latest reading found, as document.list reports it. */
export interface Glance {
  status: 'read' | 'failed';
  kind: string | null;
  vendor: {
    id: string | null;
    /** The vendor on file it matched, else the name printed on the document. */
    name: string | null;
    printed: string | null;
    match: { result: 'matched' | 'ambiguous' | 'new'; confidence?: string } | null;
  };
  invoice_count: number;
  invoice_numbers: (string | null)[];
  total_cents: number | null;
  currency: string | null;
  invoice_date: string | null;
  due_date: string | null;
  failed_checks: string[];
  ready: boolean;
  review_required: boolean;
  read_at: string;
}

/** A row of document.list. */
export type Row = Doc & {
  split_from: SplitFrom | null;
  reading: Glance | null;
  cover: { page: number; rotation: number };
};

export type View = 'table' | 'board';

/** What a person still has to do for a document in To review, in words. */
export function needs(d: Row): string[] {
  const r = d.reading;
  if (!r) return [];
  if (r.status === 'failed') return ["Couldn't be read"];
  const out: string[] = [];
  if (r.kind && r.kind !== 'invoice') out.push(r.kind === 'statement' ? 'Looks like a statement' : `Looks like a ${r.kind.replace('_', ' ')}`);
  if (r.invoice_count > 1) out.push(`Split into ${r.invoice_count} bills`);
  if (r.vendor.match?.result === 'new') out.push('New vendor');
  if (r.vendor.match?.result === 'ambiguous') out.push('Choose the vendor');
  for (const c of r.failed_checks) {
    if (c === 'one_invoice' || (c === 'vendor' && r.vendor.match?.result !== 'matched') || (c === 'is_invoice' && r.kind !== 'invoice')) continue;
    out.push(c === 'not_a_duplicate' ? 'Possible duplicate' : `Check: ${CHECK[c] ?? c}`);
  }
  if (r.review_required && !out.length) out.push('Review the lines');
  return out;
}

/** Where a document in To review sits on the board. */
export function lane(d: Row): 'reading' | 'needs' | 'ready' {
  if (!d.reading) return 'reading';
  return d.reading.ready && d.reading.invoice_count <= 1 ? 'ready' : 'needs';
}

/** "R251006156", or "R251000248 – R251006156" across several. */
function numbers(r: Glance): string {
  const n = r.invoice_numbers.filter((x): x is string => !!x);
  if (!n.length) return '';
  if (n.length === 1) return n[0]!;
  const sorted = [...n].sort();
  return `${sorted[0]} – ${sorted[sorted.length - 1]}`;
}

/**
 * The document's first page, small; an image document is its own cover.
 * Fetched with the client header like every signed-in call (an <img> can't
 * send one), and only once it scrolls into view.
 */
export function Cover({ doc }: { doc: Row }) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const box = useRef<HTMLSpanElement>(null);
  const pdf = doc.content_type === 'application/pdf';
  const url = `/api/documents/${doc.id}/cover${pdf ? `?page=${doc.cover.page}` : ''}`;
  useEffect(() => {
    if (doc.page_count === 0 || !box.current) return;
    const abort = new AbortController();
    let made: string | null = null;
    const load = () => {
      fetch(url, { signal: abort.signal, credentials: 'same-origin', headers: { 'x-payvy-client': 'web' } })
        .then(r => (r.ok ? r.blob() : Promise.reject(new Error(String(r.status)))))
        .then(b => { made = URL.createObjectURL(b); setSrc(made); })
        .catch(() => { if (!abort.signal.aborted) setFailed(true); });
    };
    const seen = typeof IntersectionObserver === 'undefined' ? null : new IntersectionObserver(entries => {
      if (entries.some(e => e.isIntersecting)) { seen?.disconnect(); load(); }
    }, { rootMargin: '200px' });
    if (seen) seen.observe(box.current); else load();
    return () => { seen?.disconnect(); abort.abort(); if (made) URL.revokeObjectURL(made); };
  }, [url, doc.page_count]);
  const glyph = failed || doc.page_count === 0 || !src;
  return (
    <span ref={box} className={`payvy-cover payvy-cover-card${glyph ? ' payvy-cover-glyph' : ''}`} aria-hidden="true">
      {glyph ? (pdf ? 'PDF' : 'IMG') : (
        <img src={src} alt="" decoding="async"
          style={doc.cover.rotation ? { transform: `rotate(${doc.cover.rotation}deg)` } : undefined} />
      )}
    </span>
  );
}

function VendorCell({ r }: { r: Glance | null }) {
  if (!r) return <span className="payvy-muted">Reading…</span>;
  if (r.status === 'failed') return <span className="payvy-muted">Not read</span>;
  const m = r.vendor.match?.result;
  return (
    <span className="payvy-inbox-vendor">
      <strong>{r.vendor.name ?? 'Unknown vendor'}</strong>
      {m === 'new' && <span className="payvy-chip payvy-chip-warn">New</span>}
      {m === 'ambiguous' && <span className="payvy-chip payvy-chip-warn">Which one?</span>}
      {m === 'matched' && r.vendor.match?.confidence === 'low' && <span className="payvy-chip">Unsure</span>}
    </span>
  );
}

function Needs({ d }: { d: Row }) {
  if (d.status !== 'inbox') return null;
  if (!d.reading) return <span className="payvy-chip">Reading</span>;
  const n = needs(d);
  if (!n.length) return <span className="payvy-chip payvy-chip-ok">Ready</span>;
  return <span className="payvy-chips">{n.map(t => <span key={t} className="payvy-chip payvy-chip-warn">{t}</span>)}</span>;
}

/** A long filename can break after _ or -, and nowhere else. */
function filenameBreaks(name: string) {
  return name.split(/([_-])/).map((bit, i) => (bit === '_' || bit === '-' ? <span key={i}>{bit}<wbr /></span> : bit));
}

/** The filename, and for a split part, where it came from with Undo. The board card is itself the link, so it shows the name as text. */
function FileCell({ d, undoSplit, linked = true }: { d: Row; undoSplit: (d: Row) => void; linked?: boolean }) {
  const name = filenameBreaks(d.filename);
  return (
    <span className="payvy-inbox-file" title={d.filename}>
      {linked ? <Link href={`/inbox/${d.id}`}>{name}</Link> : name}
      {d.split_from && (
        <span className="payvy-split-from">
          <span>Split from {d.split_from.filename ?? 'another document'}</span>
          {d.status === 'inbox' && (
            <Allowed command="document.unsplit">
              <button type="button" className="payvy-text-button" aria-label={`Undo split of ${d.filename}`} onClick={() => undoSplit(d)}>Undo</button>
            </Allowed>
          )}
        </span>
      )}
    </span>
  );
}

type SortKey = 'vendor' | 'amount' | 'due' | 'received';
const SORTS: Record<SortKey, (a: Row, b: Row) => number> = {
  vendor: (a, b) => (a.reading?.vendor.name ?? '~').localeCompare(b.reading?.vendor.name ?? '~'),
  amount: (a, b) => (a.reading?.total_cents ?? -1) - (b.reading?.total_cents ?? -1),
  due: (a, b) => (a.reading?.due_date ?? '9999').localeCompare(b.reading?.due_date ?? '9999'),
  received: (a, b) => a.received_at.localeCompare(b.received_at),
};

function SortHeader({ k, label, sort, setSort, className }: { k: SortKey; label: string; sort: [SortKey, 1 | -1]; setSort: (s: [SortKey, 1 | -1]) => void; className?: string }) {
  const active = sort[0] === k;
  return (
    <th className={className} aria-sort={active ? (sort[1] === 1 ? 'ascending' : 'descending') : 'none'}>
      <button type="button" className="payvy-sort" onClick={() => setSort([k, active ? (sort[1] === 1 ? -1 : 1) : 1])}>
        {label}<span aria-hidden="true">{active ? (sort[1] === 1 ? ' ↑' : ' ↓') : ''}</span>
      </button>
    </th>
  );
}

/** Spreadsheet-style: one row per file, with what was read from it. */
export function InboxTable({ docs, undoSplit }: { docs: Row[]; undoSplit: (d: Row) => void }) {
  const [sort, setSort] = useState<[SortKey, 1 | -1]>(['received', 1]);
  const rows = useMemo(() => [...docs].sort((a, b) => SORTS[sort[0]](a, b) * sort[1]), [docs, sort]);
  return (
    <div className="payvy-inbox-scroll">
      <table className="payvy-inbox-table">
        <thead><tr>
          <SortHeader k="vendor" label="Vendor" sort={sort} setSort={setSort} />
          <th>Invoices</th>
          <SortHeader k="amount" label="Amount" sort={sort} setSort={setSort} className="payvy-num" />
          <SortHeader k="due" label="Due" sort={sort} setSort={setSort} />
          <th>Needs</th>
          <SortHeader k="received" label="Received" sort={sort} setSort={setSort} />
        </tr></thead>
        <tbody>
          {rows.map(d => {
            const r = d.reading;
            const due = dueLabel(r?.due_date);
            return (
              <tr key={d.id}>
                <td><VendorCell r={r} /><FileCell d={d} undoSplit={undoSplit} /></td>
                <td>{r && r.invoice_count > 0 ? <><span className="payvy-inbox-count">{r.invoice_count === 1 ? '1 invoice' : `${r.invoice_count} invoices`}</span><span className="payvy-muted payvy-mono">{numbers(r)}</span></> : <span className="payvy-muted">—</span>}</td>
                <td className="payvy-num">{r?.total_cents != null ? money(r.total_cents) : <span className="payvy-muted">—</span>}</td>
                <td>{r?.due_date ? <>{date(r.due_date)}{due && d.status === 'inbox' && <span className={`payvy-due payvy-due-${due.tone}`}>{due.text}</span>}</> : <span className="payvy-muted">—</span>}</td>
                <td><Needs d={d} />{d.trashed_reason && <span className="payvy-muted">{d.trashed_reason}</span>}</td>
                <td><span>{date(d.received_at)}</span><span className="payvy-muted">{d.source === 'email' ? 'Email' : 'Upload'}</span></td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function Card({ d, undoSplit }: { d: Row; undoSplit: (d: Row) => void }) {
  const r = d.reading;
  const due = dueLabel(r?.due_date);
  return (
    <li className="payvy-inbox-card">
      <Link href={`/inbox/${d.id}`} className="payvy-inbox-card-open"><span className="payvy-sr-only">{d.filename}</span></Link>
      <Cover doc={d} />
      <div>
        <VendorCell r={r} />
        {r?.total_cents != null && <span className="payvy-inbox-amount">{money(r.total_cents)}</span>}
        {r && r.invoice_count > 1 && <span className="payvy-muted">{r.invoice_count} invoices · {numbers(r)}</span>}
        {r && r.invoice_count === 1 && <span className="payvy-muted payvy-mono">{numbers(r)}</span>}
        {r?.due_date && <span className="payvy-inbox-due-line payvy-muted"><span>Due {date(r.due_date)}</span>{due && d.status === 'inbox' && <span className={`payvy-due payvy-due-${due.tone}`}>{due.text}</span>}</span>}
        <Needs d={d} />
        <FileCell d={d} undoSplit={undoSplit} linked={false} />
      </div>
    </li>
  );
}

function Lane({ title, hint, docs, children }: { title: string; hint: string; docs: Row[]; children?: ReactNode }) {
  return (
    <section className="payvy-inbox-lane" aria-label={title}>
      <header><h3>{title}</h3><span className="payvy-inbox-lane-count">{docs.length}</span></header>
      <p className="payvy-muted">{hint}</p>
      {children}
    </section>
  );
}

/** To review grouped by what each file needs next, then what became bills. */
export function InboxBoard({ docs, filed, undoSplit }: { docs: Row[]; filed: Row[] | null; undoSplit: (d: Row) => void }) {
  const by = { reading: [] as Row[], needs: [] as Row[], ready: [] as Row[] };
  for (const d of docs) by[lane(d)].push(d);
  const cards = (list: Row[]) => list.length ? <ul>{list.map(d => <Card key={d.id} d={d} undoSplit={undoSplit} />)}</ul> : <p className="payvy-inbox-lane-empty">Nothing here</p>;
  return (
    <div className="payvy-inbox-board">
      <Lane title="Reading" hint="Being read; they move on by themselves." docs={by.reading}>{cards(by.reading)}</Lane>
      <Lane title="Needs you" hint="A new vendor, a split, or a check to look at." docs={by.needs}>{cards(by.needs)}</Lane>
      <Lane title="Ready to create" hint="Every check passed." docs={by.ready}>{cards(by.ready)}</Lane>
      <Lane title="Became bills" hint="Filed with a bill, latest first." docs={filed ?? []}>{filed ? cards(filed) : <p className="payvy-inbox-lane-empty">Loading…</p>}</Lane>
    </div>
  );
}

/** A key typed for a field or with a modifier isn't a shortcut. */
export function typing(e: KeyboardEvent): boolean {
  if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return true;
  const t = e.target as HTMLElement | null;
  return !!t && (t.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName) || !!t.closest('[role="dialog"],[role="alertdialog"],[role="listbox"],[role="menu"]'));
}

const VIEW_KEY = 'payvy.inbox.view';
/** The chosen view, remembered in this browser; T and B switch it. */
export function useInboxView(): [View, (v: View) => void] {
  const [view, setViewState] = useState<View>(() => {
    try { return localStorage.getItem(VIEW_KEY) === 'board' ? 'board' : 'table'; } catch { return 'table'; }
  });
  const setView = (v: View) => {
    setViewState(v);
    try { localStorage.setItem(VIEW_KEY, v); } catch { /* storage may be blocked */ }
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (typing(e)) return;
      const k = e.key.toLowerCase();
      if (k !== 't' && k !== 'b') return;
      e.preventDefault();
      setView(k === 'b' ? 'board' : 'table');
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });
  return [view, setView];
}
