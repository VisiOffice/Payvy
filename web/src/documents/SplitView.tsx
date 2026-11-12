// A file of several invoices, laid out as one card per invoice. Pages move
// between cards by dragging (pointer or keyboard) or with each page's Move
// menu, and "Create N bills" splits the file and makes a bill of each part
// that reads cleanly.
import { useEffect, useRef, useState, type ReactNode } from 'react';
import {
  closestCorners,
  DndContext,
  DragOverlay,
  KeyboardSensor,
  PointerSensor,
  useDroppable,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragOverEvent,
  type DragStartEvent,
} from '@dnd-kit/core';
import { SortableContext, arrayMove, sortableKeyboardCoordinates, useSortable, rectSortingStrategy } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { Button } from '@astryxdesign/core/Button';
import { Dialog, DialogHeader } from '@astryxdesign/core/Dialog';
import { DropdownMenu } from '@astryxdesign/core/DropdownMenu';
import { HStack } from '@astryxdesign/core/HStack';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import type { CommandInputs } from '../api/commands.gen';
import { money } from '../money';
import { describePages, type PageRef } from './pages';
import type { DocumentDetail, SuggestedInvoice } from './types';

/** One invoice's pages as laid out here; `read` is what the reader found for them, if they're unchanged. */
export interface Group {
  key: string;
  pages: number[];
  read: SuggestedInvoice | null;
}

export interface Layout {
  groups: Group[];
  /** Pages that are part of no bill: they stay in this document. */
  leftover: number[];
}

const LEFTOVER = 'leftover';
const NEW = 'new-invoice';
const pageId = (p: number) => `page-${p}`;
const pageOf = (id: string) => Number(id.slice(5));

export const invoiceName = (g: { read: SuggestedInvoice | null }, i: number) =>
  g.read?.invoice_number ? `Invoice ${g.read.invoice_number}` : `Invoice ${i + 1}`;

/** The reader's invoices over the visible pages; anything they don't cover is left over. */
export function layoutOf(invoices: SuggestedInvoice[], visible: number[]): Layout {
  const groups = invoices.map((g, i) => ({ key: `g${i}`, pages: [...g.pages], read: g }));
  const taken = new Set(groups.flatMap((g) => g.pages));
  return { groups, leftover: visible.filter((p) => !taken.has(p)) };
}

/** A moved page drops what was read for both invoices it touched: their totals no longer apply. */
function same(a: number[], b: number[]) {
  return a.length === b.length && a.every((p, i) => p === b[i]);
}
function reconcile(l: Layout, invoices: SuggestedInvoice[]): Layout {
  return {
    leftover: l.leftover,
    groups: l.groups
      .filter((g) => g.pages.length > 0)
      .map((g) => ({ ...g, read: invoices.find((r) => same(r.pages, g.pages)) ?? null })),
  };
}

/** Where a layout is kept while a person works on it, per document and page version. */
const keyOf = (id: string, version: number) => `payvy.split.${id}.${version}`;
function remembered(id: string, version: number, visible: number[], invoices: SuggestedInvoice[]): Layout | null {
  try {
    const raw = window.localStorage.getItem(keyOf(id, version));
    if (!raw) return null;
    const l = JSON.parse(raw) as { groups: number[][]; leftover: number[] };
    const all = [...l.groups.flat(), ...l.leftover].sort((a, b) => a - b);
    if (!same(all, [...visible].sort((a, b) => a - b))) return null;
    return reconcile({ groups: l.groups.map((pages, i) => ({ key: `m${i}`, pages, read: null })), leftover: l.leftover }, invoices);
  } catch {
    return null;
  }
}
function remember(id: string, version: number, l: Layout | null) {
  try {
    if (l) window.localStorage.setItem(keyOf(id, version), JSON.stringify({ groups: l.groups.map((g) => g.pages), leftover: l.leftover }));
    else window.localStorage.removeItem(keyOf(id, version));
  } catch {
    // Not remembered in a private window; the layout still works.
  }
}

/** The header line: how the pages became invoices, and what's left over. */
export function explain(pageCount: number, l: Layout): string {
  const n = l.groups.length;
  const parts = [`${pageCount} ${pageCount === 1 ? 'page' : 'pages'} → ${n} ${n === 1 ? 'invoice' : 'invoices'}`];
  const long = l.groups.map((g, i) => ({ g, i })).filter(({ g }) => g.pages.length > 1);
  if (long.length > 0 && long.length <= 2) {
    for (const { g, i } of long) parts.push(`${g.read?.invoice_number ?? `Invoice ${i + 1}`} runs ${g.pages.length} pages`);
  } else if (long.length > 2) {
    parts.push(`${long.length} invoices run more than one page`);
  }
  const left = l.leftover.length;
  parts.push(left === 0 ? 'nothing left over' : `${left} ${left === 1 ? 'page isn’t' : 'pages aren’t'} part of any bill`);
  return parts.join(' · ');
}

/** A page's first-page image from the server, fetched with the client header the cookie needs. */
function useCover(documentId: string, page: number, pdf: boolean) {
  const [src, setSrc] = useState<string | null>(null);
  const box = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const abort = new AbortController();
    let made: string | null = null;
    const load = () =>
      fetch(`/api/documents/${documentId}/cover${pdf ? `?page=${page}` : ''}`, {
        signal: abort.signal,
        credentials: 'same-origin',
        headers: { 'x-payvy-client': 'web' },
      })
        .then((r) => (r.ok ? r.blob() : Promise.reject(new Error(String(r.status)))))
        .then((b) => {
          made = URL.createObjectURL(b);
          setSrc(made);
        })
        .catch(() => {});
    const seen = new IntersectionObserver((es) => {
      if (es.some((e) => e.isIntersecting)) {
        seen.disconnect();
        void load();
      }
    }, { rootMargin: '200px' });
    seen.observe(el);
    return () => {
      seen.disconnect();
      abort.abort();
      if (made) URL.revokeObjectURL(made);
    };
  }, [documentId, page, pdf]);
  return { src, box };
}

interface Destination {
  label: string;
  to: string;
}

function Thumb({ documentId, pdf, page, rotation }: { documentId: string; pdf: boolean; page: number; rotation: number }) {
  const { src, box } = useCover(documentId, page, pdf);
  return (
    <span ref={box} className="payvy-split-thumb" aria-hidden="true">
      {src ? <img src={src} alt="" style={rotation ? { transform: `rotate(${rotation}deg)` } : undefined} /> : <span>{page}</span>}
    </span>
  );
}

function PageTile({
  page,
  documentId,
  pdf,
  rotation,
  destinations,
  onMove,
  onOpen,
  disabled,
}: {
  page: number;
  documentId: string;
  pdf: boolean;
  rotation: number;
  destinations: Destination[];
  onMove: (page: number, to: string) => void;
  onOpen: (page: number) => void;
  disabled: boolean;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id: pageId(page), disabled });
  return (
    <li
      ref={setNodeRef}
      className="payvy-split-page"
      data-dragging={isDragging || undefined}
      style={{ transform: CSS.Transform.toString(transform), transition }}
    >
      <button
        type="button"
        className="payvy-split-handle"
        {...attributes}
        aria-label={`Page ${page}. Drag to move it, or press space to pick it up`}
        aria-roledescription="draggable page"
        {...listeners}
        onDoubleClick={() => onOpen(page)}
      >
        <Thumb documentId={documentId} pdf={pdf} page={page} rotation={rotation} />
      </button>
      <span className="payvy-split-page-foot">
        <span>p. {page}</span>
        {!disabled && (
          <DropdownMenu
            hasChevron={false}
            menuWidth={240}
            renderTrigger={(props) => (
              <button {...props} type="button" className="payvy-split-move" aria-label={`Move page ${page}`}>
                Move
              </button>
            )}
            items={destinations.map((d) => ({ label: d.label, onClick: () => onMove(page, d.to) }))}
          />
        )}
      </span>
    </li>
  );
}

function Drop({ id, className, label, children }: { id: string; className: string; label: string; children: ReactNode }) {
  const { setNodeRef, isOver } = useDroppable({ id });
  return (
    <section ref={setNodeRef} className={className} data-over={isOver || undefined} aria-label={label}>
      {children}
    </section>
  );
}

export function SplitView({
  detail,
  invoices,
  vendorName,
  canCreate,
  onReview,
  onOpenPage,
  onCount,
  onDone,
}: {
  detail: DocumentDetail;
  invoices: SuggestedInvoice[];
  vendorName: string | null;
  canCreate: boolean;
  /** Review one invoice by itself, in the form beside. */
  onReview: (pages: number[], read: SuggestedInvoice | null) => void;
  onOpenPage: (page: number) => void;
  /** How many invoices the cards hold now, for the summary beside. */
  onCount: (n: number) => void;
  /** After Create: the document again, with the batch's progress. */
  onDone: () => void;
}) {
  const doc = detail.document;
  const pdf = doc.content_type === 'application/pdf';
  const arrangement: PageRef[] = (detail.pages.arrangement ?? []).filter((p) => !p.hidden);
  const visible = arrangement.map((p) => p.page);
  const rotation = (p: number) => arrangement.find((r) => r.page === p)?.rotation ?? 0;
  const version = detail.pages.version;
  const locked = detail.pages.locked || doc.status !== 'inbox';

  const [layout, setLayout] = useState<Layout>(
    () => remembered(doc.id, version, visible, invoices) ?? layoutOf(invoices, visible),
  );
  const [dragging, setDragging] = useState<number | null>(null);
  const before = useRef<Layout | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // One id per layout: a retry or double click of the same layout is the
  // same request, which the server answers with the batch it already made.
  const request = useRef<string>(crypto.randomUUID());
  const [said, setSaid] = useState('');
  const counter = useRef(0);

  useEffect(() => onCount(layout.groups.length), [layout.groups.length, onCount]);

  const update = (next: Layout) => {
    const l = reconcile(next, invoices);
    setLayout(l);
    request.current = crypto.randomUUID();
    const fresh = layoutOf(invoices, visible);
    remember(doc.id, version, same(l.groups.flatMap((g) => g.pages), fresh.groups.flatMap((g) => g.pages)) && l.groups.length === fresh.groups.length ? null : l);
  };

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  /** Which box a page or box id belongs to. */
  function boxOf(id: string, l: Layout = layout): string | null {
    if (id === LEFTOVER || id === NEW || l.groups.some((g) => g.key === id)) return id;
    const p = pageOf(id);
    if (l.leftover.includes(p)) return LEFTOVER;
    return l.groups.find((g) => g.pages.includes(p))?.key ?? null;
  }

  /** The layout with `page` taken out of wherever it is and put in `to`, at `index` (the end if absent). */
  function moved(l: Layout, page: number, to: string, index?: number): Layout {
    const groups = l.groups.map((g) => ({ ...g, pages: g.pages.filter((p) => p !== page) }));
    let leftover = l.leftover.filter((p) => p !== page);
    const put = (list: number[]) => {
      const at = index === undefined ? list.length : Math.max(0, Math.min(index, list.length));
      return [...list.slice(0, at), page, ...list.slice(at)];
    };
    if (to === LEFTOVER) leftover = put(leftover);
    else if (to === NEW) groups.push({ key: `n${++counter.current}`, pages: [page], read: null });
    else {
      const g = groups.find((x) => x.key === to);
      if (g) g.pages = put(g.pages);
    }
    return { groups, leftover };
  }

  const nameOf = (key: string) => {
    if (key === LEFTOVER) return 'Not part of any bill';
    if (key === NEW) return 'a new invoice';
    const i = layout.groups.findIndex((g) => g.key === key);
    return i < 0 ? 'an invoice' : invoiceName(layout.groups[i]!, i);
  };

  function move(page: number, to: string) {
    const next = moved(layout, page, to);
    update(next);
    setSaid(`Page ${page} moved to ${to === NEW ? 'a new invoice' : nameOf(to)}`);
  }

  function onDragStart(e: DragStartEvent) {
    setDragging(pageOf(String(e.active.id)));
    before.current = layout;
  }
  // Across boxes as it goes, so the page shows where it would land.
  function onDragOver(e: DragOverEvent) {
    const over = e.over && String(e.over.id);
    if (!over) return;
    const page = pageOf(String(e.active.id));
    const from = boxOf(String(e.active.id));
    const to = boxOf(over);
    if (!from || !to || from === to || to === NEW) return;
    const list = to === LEFTOVER ? layout.leftover : (layout.groups.find((g) => g.key === to)?.pages ?? []);
    const at = over.startsWith('page-') ? list.indexOf(pageOf(over)) : list.length;
    setLayout(moved(layout, page, to, at));
  }
  function onDragEnd(e: DragEndEvent) {
    setDragging(null);
    const over = e.over && String(e.over.id);
    const page = pageOf(String(e.active.id));
    if (!over) return update(layout);
    const to = boxOf(over);
    if (to === NEW) {
      setSaid(`Page ${page} starts a new invoice`);
      return update(moved(layout, page, NEW));
    }
    const from = boxOf(String(e.active.id));
    if (from && from === to && over.startsWith('page-')) {
      const list = from === LEFTOVER ? layout.leftover : layout.groups.find((g) => g.key === from)!.pages;
      const a = list.indexOf(page);
      const b = list.indexOf(pageOf(over));
      const sorted = arrayMove(list, a, b);
      return update(
        from === LEFTOVER
          ? { ...layout, leftover: sorted }
          : { ...layout, groups: layout.groups.map((g) => (g.key === from ? { ...g, pages: sorted } : g)) },
      );
    }
    update(layout);
  }

  const destinations = (page: number): Destination[] => {
    const here = boxOf(pageId(page));
    return [
      ...layout.groups.map((g, i) => ({ label: `To ${invoiceName(g, i)}`, to: g.key })),
      { label: 'To a new invoice', to: NEW },
      { label: 'Not part of any bill', to: LEFTOVER },
    ].filter((d) => d.to !== here);
  };

  const totals = layout.groups.map((g) => g.read?.total_cents ?? null);
  const known = totals.filter((t): t is number => t !== null);
  const total = known.reduce((a, b) => a + b, 0);
  const n = layout.groups.length;
  const createLabel = `Create ${n} ${n === 1 ? 'bill' : 'bills'}`;

  async function createAll() {
    setBusy(true);
    setError(null);
    try {
      await run('document.create_bills', {
        document_id: doc.id,
        expected_version: version,
        request_id: request.current,
        groups: layout.groups.map((g) => g.pages) as CommandInputs['document.create_bills']['groups'],
        leftover: layout.leftover,
      });
      remember(doc.id, version, null);
      setConfirming(false);
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="payvy-split-view">
      <header className="payvy-split-header">
        <div>
          <strong>{vendorName ? `${vendorName} · ` : ''}{n} {n === 1 ? 'invoice' : 'invoices'} in this file</strong>
          <p className="payvy-split-explain" data-leftover={layout.leftover.length > 0 || undefined}>{explain(visible.length, layout)}</p>
        </div>
        {canCreate && !locked && (
          <Button label={createLabel} variant="primary" isDisabled={n === 0 || busy} onClick={() => setConfirming(true)} />
        )}
      </header>
      {error && <p role="alert" className="payvy-split-error">{error}</p>}
      <p className="payvy-sr-only" aria-live="polite">{said}</p>
      <DndContext sensors={sensors} collisionDetection={closestCorners} onDragStart={onDragStart} onDragOver={onDragOver} onDragEnd={onDragEnd} onDragCancel={() => { setDragging(null); if (before.current) setLayout(before.current); }}>
        <ul className="payvy-split-grid" aria-label="Invoices in this file">
          {layout.groups.map((g, i) => (
            <li key={g.key} className="payvy-split-card-item">
              <Drop id={g.key} className="payvy-split-card" label={invoiceName(g, i)}>
                <header>
                  <strong>{invoiceName(g, i)}</strong>
                  <span className="payvy-muted">
                    {describePages(g.pages)}
                    {g.read?.total_cents != null && ` · ${money(g.read.total_cents)}`}
                    {!g.read && ' · read when created'}
                  </span>
                </header>
                <SortableContext items={g.pages.map(pageId)} strategy={rectSortingStrategy}>
                  <ol className="payvy-split-pages" aria-label={`Pages of ${invoiceName(g, i)}`}>
                    {g.pages.map((p) => (
                      <PageTile key={p} page={p} documentId={doc.id} pdf={pdf} rotation={rotation(p)} destinations={destinations(p)} onMove={move} onOpen={onOpenPage} disabled={locked} />
                    ))}
                  </ol>
                </SortableContext>
                {!locked && (
                  <Button label={`Select ${invoiceName(g, i)}`} size="sm" variant="ghost" onClick={() => onReview(g.pages, g.read)}>
                    Review this one
                  </Button>
                )}
              </Drop>
            </li>
          ))}
          {!locked && (
            <li className="payvy-split-card-item">
              <Drop id={NEW} className="payvy-split-card payvy-split-new" label="New invoice">
                <span>Drop a page here to start a new invoice</span>
              </Drop>
            </li>
          )}
        </ul>
        <Drop id={LEFTOVER} className="payvy-split-leftover" label="Not part of any bill">
          <header>
            <strong>Not part of any bill</strong>
            <span className="payvy-muted">
              {layout.leftover.length === 0 ? 'Drop a page here to keep it out of every bill. It stays in To review.' : `${describePages(layout.leftover)} stay in To review`}
            </span>
          </header>
          <SortableContext items={layout.leftover.map(pageId)} strategy={rectSortingStrategy}>
            <ol className="payvy-split-pages" aria-label="Pages not part of any bill">
              {layout.leftover.map((p) => (
                <PageTile key={p} page={p} documentId={doc.id} pdf={pdf} rotation={rotation(p)} destinations={destinations(p)} onMove={move} onOpen={onOpenPage} disabled={locked} />
              ))}
            </ol>
          </SortableContext>
        </Drop>
        <DragOverlay>{dragging !== null ? <span className="payvy-split-page payvy-split-lifted"><Thumb documentId={doc.id} pdf={pdf} page={dragging} rotation={rotation(dragging)} /></span> : null}</DragOverlay>
      </DndContext>

      {confirming && (
        <Dialog isOpen onOpenChange={(open) => !open && setConfirming(false)} width={480} purpose="form">
          <DialogHeader title={`${createLabel}?`} subtitle={known.length ? `${money(total)} as read${known.length < n ? `, for ${known.length} of ${n}` : ''}` : undefined} onOpenChange={(open) => !open && setConfirming(false)} />
          <VStack gap={4} padding={4}>
            <p>
              The file is split now, one document per invoice. Payvy then reads each one and makes it a bill on your
              workflow when it reads cleanly; the rest wait in To review for you. A copy of an invoice you already have
              stacks on that bill instead of becoming a second one. You can leave this page while it works.
            </p>
            {error && <p role="alert" className="payvy-split-error">{error}</p>}
            {layout.leftover.length > 0 && <p>{describePages(layout.leftover)} {layout.leftover.length === 1 ? 'isn’t' : 'aren’t'} part of any bill and stay here.</p>}
            <HStack gap={2} hAlign="end">
              <Button label="Cancel" variant="ghost" onClick={() => setConfirming(false)} />
              <Button label={createLabel} variant="primary" isDisabled={busy} clickAction={createAll} />
            </HStack>
          </VStack>
        </Dialog>
      )}

    </div>
  );
}
