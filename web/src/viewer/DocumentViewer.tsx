import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode, type SVGProps } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Icon } from '@astryxdesign/core/Icon';
import { Kbd } from '@astryxdesign/core/Kbd';
import { Spinner } from '@astryxdesign/core/Spinner';
import { VStack } from '@astryxdesign/core/VStack';
import { find, type Item, type Rect } from './match';

export interface Highlight {
  key: string;
  label: string;
  /** Ways the value may be printed; the first found wins. */
  candidates: string[];
}

/** One page of a document's arrangement: a page of the original, by number. */
export interface PageRef {
  page: number;
  /** Degrees clockwise: 0, 90, 180 or 270. */
  rotation: number;
  hidden: boolean;
}

/** A page of the original file, upright. */
export interface Page {
  width: number;
  height: number;
  /** Renders at a scale and extra rotation into a canvas; cancel before rendering again. */
  render: (canvas: HTMLCanvasElement, scale: number, rotation: number) => { done: Promise<void>; cancel: () => void };
  items: Item[];
}

/** The file through the server (same origin, signed in), as bytes. */
async function fetchFile(documentId: string, signal: AbortSignal): Promise<ArrayBuffer> {
  const res = await fetch(`/api/documents/${documentId}/file`, {
    signal,
    credentials: 'same-origin',
    headers: { 'x-payvy-client': 'web' },
  });
  if (!res.ok) throw new Error(`Couldn't load the document (${res.status})`);
  return res.arrayBuffer();
}

async function loadPdf(data: ArrayBuffer): Promise<Page[]> {
  // pdf.js is large; load it only when a PDF is opened.
  const pdfjs = await import('pdfjs-dist');
  const worker = await import('pdfjs-dist/build/pdf.worker.min.mjs?url');
  pdfjs.GlobalWorkerOptions.workerSrc = worker.default;
  const doc = await pdfjs.getDocument({ data }).promise;
  const pages: Page[] = [];
  for (let n = 1; n <= doc.numPages; n++) {
    const page = await doc.getPage(n);
    const base = page.getViewport({ scale: 1 });
    const content = await page.getTextContent();
    const items: Item[] = [];
    for (const it of content.items) {
      if (!('str' in it) || !it.str.trim()) continue;
      const t = pdfjs.Util.transform(base.transform, it.transform);
      const h = Math.hypot(t[2]!, t[3]!);
      items.push({ str: it.str, x: t[4]!, y: t[5]! - h, w: it.width, h });
    }
    pages.push({
      width: base.width,
      height: base.height,
      items,
      render: (canvas, scale, rotation) => {
        const ratio = window.devicePixelRatio || 1;
        const viewport = page.getViewport({ scale: scale * ratio, rotation: (page.rotate + rotation) % 360 });
        canvas.width = viewport.width;
        canvas.height = viewport.height;
        const task = page.render({ canvas, viewport });
        return { done: task.promise, cancel: () => task.cancel() };
      },
    });
  }
  return pages;
}

/** A photo or scan is one page, drawn like a PDF page so it turns the same way. */
async function loadImage(data: ArrayBuffer, contentType: string): Promise<Page[]> {
  const bitmap = await createImageBitmap(new Blob([data], { type: contentType }));
  return [
    {
      width: bitmap.width,
      height: bitmap.height,
      items: [],
      render: (canvas, scale, rotation) => {
        const ratio = window.devicePixelRatio || 1;
        const s = scale * ratio;
        const turned = rotation === 90 || rotation === 270;
        canvas.width = Math.round((turned ? bitmap.height : bitmap.width) * s);
        canvas.height = Math.round((turned ? bitmap.width : bitmap.height) * s);
        const ctx = canvas.getContext('2d');
        if (ctx) {
          ctx.translate(canvas.width / 2, canvas.height / 2);
          ctx.rotate((rotation * Math.PI) / 180);
          ctx.drawImage(bitmap, (-bitmap.width * s) / 2, (-bitmap.height * s) / 2, bitmap.width * s, bitmap.height * s);
        }
        return { done: Promise.resolve(), cancel: () => {} };
      },
    },
  ];
}

const TIMEOUT = 'The original is taking too long to load. Try opening it again.';

/**
 * The pages of a document's original file. The viewer and the page strip
 * share one load, so the file is fetched once; a failed load is forgotten so
 * it can be retried.
 */
const loads = new Map<string, Promise<Page[]>>();

export function loadDocument(documentId: string, contentType: string): Promise<Page[]> {
  const cached = loads.get(documentId);
  if (cached) return cached;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30_000);
  const load = fetchFile(documentId, controller.signal)
    .then((data) => (contentType === 'application/pdf' ? loadPdf(data) : loadImage(data, contentType)))
    .catch((e: unknown) => {
      loads.delete(documentId);
      throw e instanceof Error && e.name === 'AbortError' ? new Error(TIMEOUT) : e;
    })
    .finally(() => clearTimeout(timeout));
  loads.set(documentId, load);
  // Keep the last few documents only.
  while (loads.size > 3) loads.delete(loads.keys().next().value!);
  return load;
}

/** A page's size once turned. */
export function turnedSize(page: Page, rotation: number): { width: number; height: number } {
  return rotation === 90 || rotation === 270
    ? { width: page.height, height: page.width }
    : { width: page.width, height: page.height };
}

/** A box on the upright page, where it lands once the page is turned clockwise. */
function turnRect(r: Rect, page: Page, rotation: number): Rect {
  const { width: w, height: h } = page;
  switch (rotation) {
    case 90:
      return { x: h - r.y - r.h, y: r.x, w: r.h, h: r.w };
    case 180:
      return { x: w - r.x - r.w, y: h - r.y - r.h, w: r.w, h: r.h };
    case 270:
      return { x: r.y, y: w - r.x - r.w, w: r.h, h: r.w };
    default:
      return r;
  }
}

type Loaded =
  | { kind: 'loading' }
  | { kind: 'ready'; pages: Page[] }
  | { kind: 'error'; message: string };

/** The pages of a document as they load. */
export function useDocumentPages(documentId: string, contentType: string, attempt = 0): Loaded {
  const [loaded, setLoaded] = useState<Loaded>({ kind: 'loading' });
  useEffect(() => {
    let cancelled = false;
    setLoaded({ kind: 'loading' });
    loadDocument(documentId, contentType)
      .then((pages) => !cancelled && setLoaded({ kind: 'ready', pages }))
      .catch((e: Error) => !cancelled && setLoaded({ kind: 'error', message: e.message }));
    return () => {
      cancelled = true;
    };
  }, [documentId, contentType, attempt]);
  return loaded;
}

/** What the viewer shows: each visible page of the arrangement, in order. */
interface Shown {
  ref: PageRef;
  page: Page;
}

const ZOOMS = [0.5, 0.67, 0.8, 1, 1.25, 1.5, 2, 3];

/** One page at a time, moved through with buttons and the keyboard. */
export interface Paging {
  /** The page shown, by its number in the original; null for the first. */
  current: number | null;
  onCurrentChange: (page: number) => void;
  /** More controls for the toolbar, for the page shown (e.g. its tick box). */
  extra?: (ref: PageRef) => ReactNode;
}

/**
 * Where a key press belongs to something else: typing, a menu or list, a
 * radio group (arrows move its choice), or an open dialog.
 */
function keyIsTaken(e: KeyboardEvent): boolean {
  if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey || e.shiftKey || e.isComposing) return true;
  const t = e.target instanceof Element ? e.target : null;
  if (t) {
    // A tick box takes neither letters nor arrows, so flipping on from one is fine.
    if (t.closest('input:not([type="checkbox"]), textarea, select, [contenteditable]:not([contenteditable="false"])')) return true;
    if (
      t.closest(
        '[role="combobox"], [role="listbox"], [role="option"], [role="menu"], [role="menuitem"], [role="radiogroup"], [role="radio"], [role="tablist"], [role="slider"], [role="spinbutton"], [role="textbox"], [role="grid"], [role="tree"], [role="dialog"], [role="alertdialog"], dialog',
      )
    )
      return true;
  }
  return !!document.querySelector('dialog[open], [role="dialog"], [role="alertdialog"], [aria-modal="true"]');
}

/**
 * A document with zoom and highlights of what was read from it. Clicking a
 * field elsewhere sets `focus`, which scrolls to it and pulses it. With an
 * `arrangement`, pages show in its order and rotation, and hidden ones are
 * left out; the original file is unchanged. With `paging`, one page shows at
 * a time, fitted to the panel, with Previous and Next (← → or J K).
 */
export function DocumentViewer({
  documentId,
  contentType,
  highlights = [],
  focus,
  arrangement,
  paging,
}: {
  documentId: string;
  contentType: string;
  highlights?: Highlight[];
  focus?: string | null;
  arrangement?: PageRef[] | null;
  paging?: Paging;
}) {
  const [attempt, setAttempt] = useState(0);
  const loaded = useDocumentPages(documentId, contentType, attempt);
  const [scale, setScale] = useState<number | 'fit' | 'page'>('page');
  const [pageScale, setPageScale] = useState(1);
  const [fitScale, setFitScale] = useState(1);
  const scroller = useRef<HTMLDivElement>(null);

  const shown = useMemo<Shown[]>(() => {
    if (loaded.kind !== 'ready') return [];
    const refs = arrangement?.length
      ? arrangement
      : loaded.pages.map((_, i) => ({ page: i + 1, rotation: 0, hidden: false }));
    return refs.flatMap((ref) => {
      const page = loaded.pages[ref.page - 1];
      return page && !ref.hidden ? [{ ref, page }] : [];
    });
  }, [loaded, arrangement]);

  // The page shown when paging: the current one, or where it was if it left.
  const lastIndex = useRef(0);
  const at = paging ? shown.findIndex((s) => s.ref.page === paging.current) : -1;
  const index = !paging || shown.length === 0 ? 0 : at >= 0 ? at : Math.min(lastIndex.current, shown.length - 1);
  lastIndex.current = index;
  const onCurrentChange = paging?.onCurrentChange;
  const currentPage = paging ? shown[index]?.ref.page : undefined;
  useEffect(() => {
    if (paging && currentPage !== undefined && currentPage !== paging.current) onCurrentChange?.(currentPage);
  }, [paging, currentPage, onCurrentChange]);
  const visible = useMemo(() => (paging ? shown.slice(index, index + 1) : shown), [paging, shown, index]);

  const pad = paging ? 12 : 24;
  // Fit width follows the panel's width; fit page, the panel's height too.
  useEffect(() => {
    const el = scroller.current;
    if (!el || visible.length === 0) return;
    const sizes = visible.map((s) => turnedSize(s.page, s.ref.rotation));
    const widest = Math.max(...sizes.map((p) => p.width));
    const tallest = Math.max(...sizes.map((p) => p.height));
    const update = () => {
      if (!el.clientWidth || !el.clientHeight) return;
      const width = Math.max(0.25, (el.clientWidth - pad * 2) / widest);
      setFitScale(width);
      setPageScale(Math.max(0.25, Math.min(width, (el.clientHeight - pad * 2) / tallest)));
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, [visible, pad]);

  const s = scale === 'fit' ? fitScale : scale === 'page' ? pageScale : scale;
  const ready = loaded.kind === 'ready';

  const zoom = useCallback(
    (dir: 1 | -1) => {
      const current = s;
      const next = dir > 0 ? ZOOMS.find((z) => z > current + 0.01) : [...ZOOMS].reverse().find((z) => z < current - 0.01);
      if (next) setScale(next);
    },
    [s],
  );

  // Ctrl/⌘ + wheel zooms the document, not the page.
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) return;
      e.preventDefault();
      zoom(e.deltaY < 0 ? 1 : -1);
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  }, [zoom]);

  const go = useCallback(
    (to: number) => {
      const target = shown[to];
      if (!target || !onCurrentChange) return;
      onCurrentChange(target.ref.page);
    },
    [shown, onCurrentChange],
  );

  // A new page starts at its top.
  const isPaging = !!paging;
  useEffect(() => {
    if (isPaging) scroller.current?.scrollTo?.({ top: 0, left: 0 });
  }, [isPaging, currentPage]);

  // ← → and J K move through the pages, unless the key is for something else.
  const paged = !!paging && shown.length > 1;
  useEffect(() => {
    if (!paged) return;
    const onKey = (e: KeyboardEvent) => {
      const dir = e.key === 'ArrowRight' || e.key === 'j' ? 1 : e.key === 'ArrowLeft' || e.key === 'k' ? -1 : 0;
      if (!dir || keyIsTaken(e)) return;
      // Not while the viewer is out of sight (the details tab on a phone).
      if (!scroller.current || scroller.current.getClientRects().length === 0) return;
      e.preventDefault();
      go(index + dir);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [paged, go, index]);

  // A swipe across a page that fits turns it, on a touch screen.
  const swipe = useRef<{ x: number; y: number } | null>(null);

  // Highlights per shown page, found on the upright page and then turned.
  const found = useMemo(
    () =>
      shown.map(({ page, ref }) =>
        highlights.flatMap((h) =>
          find(page.items, h.candidates).map((r) => ({ ...turnRect(r, page, ref.rotation), key: h.key, label: h.label })),
        ),
      ),
    [shown, highlights],
  );

  // A focused field on another page brings that page up, once per focus.
  const brought = useRef<string | null>(null);
  useEffect(() => {
    if (!isPaging || !focus || brought.current === focus) return;
    const at = found.findIndex((rects) => rects.some((r) => r.key === focus));
    if (at < 0 || !shown[at]) return;
    brought.current = focus;
    onCurrentChange?.(shown[at].ref.page);
  }, [isPaging, focus, found, shown, onCurrentChange]);

  // Scroll the focused field into view.
  useEffect(() => {
    if (!focus || !scroller.current) return;
    const el = scroller.current.querySelector<HTMLElement>(`[data-hl="${CSS.escape(focus)}"]`);
    el?.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'center' });
  }, [focus, found, s, currentPage]);

  const hasText = ready && loaded.pages.some((p) => p.items.length > 0);
  const missing = useMemo(() => {
    if (!hasText) return [];
    const seen = new Set(found.flat().map((r) => r.key));
    return highlights.filter((h) => !seen.has(h.key));
  }, [found, highlights, hasText]);

  const hiddenCount = ready ? Math.max(0, loaded.pages.length - shown.length) : 0;
  const offset = paging ? index : 0;
  const here = paging ? shown[index] : undefined;

  const zoomControls = (
    <div className="payvy-viewer-controls">
      <div className="payvy-viewer-control-group">
        <Button
          label="Zoom out"
          tooltip="Zoom out"
          icon={<Icon icon={MinusIcon} />}
          isIconOnly={!!paging}
          size="sm"
          variant="ghost"
          isDisabled={!ready || s <= ZOOMS[0]!}
          onClick={() => zoom(-1)}
        />
        <span className="payvy-viewer-scale">{ready ? `${Math.round(s * 100)}%` : '—'}</span>
        <Button
          label="Zoom in"
          tooltip="Zoom in"
          icon={<Icon icon={PlusIcon} />}
          isIconOnly={!!paging}
          size="sm"
          variant="ghost"
          isDisabled={!ready || s >= ZOOMS[ZOOMS.length - 1]!}
          onClick={() => zoom(1)}
        />
      </div>
      <div className="payvy-viewer-control-group">
        {ready && (
          <Button label="Fit page" size="sm" variant={scale === 'page' ? 'secondary' : 'ghost'} onClick={() => setScale('page')} />
        )}
        <Button
          label="Fit width"
          size="sm"
          variant={scale === 'fit' ? 'secondary' : 'ghost'}
          isDisabled={!ready}
          onClick={() => setScale('fit')}
        />
      </div>
    </div>
  );

  return (
    <VStack height="100%" gap={0} style={{ minHeight: 0 }}>
      {paging ? (
        <div className="payvy-viewer-toolbar" data-paged="">
          {ready && shown.length > 1 && (
            <div className="payvy-viewer-nav" role="group" aria-label="Pages of the document">
              <Button
                label="Previous page"
                tooltip="Previous page (← or K)"
                icon={<Icon icon="chevronLeft" />}
                isIconOnly
                size="sm"
                variant="ghost"
                isDisabled={index === 0}
                aria-keyshortcuts="ArrowLeft K"
                onClick={() => go(index - 1)}
              />
              <span className="payvy-viewer-key" aria-hidden="true"><Kbd keys="k" /></span>
              <span className="payvy-viewer-counter" aria-live="polite" aria-atomic="true">
                Page {index + 1} of {shown.length}
              </span>
              <span className="payvy-viewer-key" aria-hidden="true"><Kbd keys="j" /></span>
              <Button
                label="Next page"
                tooltip="Next page (→ or J)"
                icon={<Icon icon="chevronRight" />}
                isIconOnly
                size="sm"
                variant="ghost"
                isDisabled={index === shown.length - 1}
                aria-keyshortcuts="ArrowRight J"
                onClick={() => go(index + 1)}
              />
            </div>
          )}
          {ready && shown.length <= 1 && (
            <span className="payvy-viewer-counter">
              {shown.length === 1 ? '1 page' : 'No pages'}
              {hiddenCount > 0 && ` · ${hiddenCount} hidden`}
            </span>
          )}
          {here && paging.extra?.(here.ref)}
          {zoomControls}
        </div>
      ) : (
        <div className="payvy-viewer-toolbar">
          <div className="payvy-viewer-title">
            <strong>Original document</strong>
            {ready && (
              <span>
                {shown.length} {shown.length === 1 ? 'page' : 'pages'}
                {hiddenCount > 0 && ` · ${hiddenCount} hidden`}
              </span>
            )}
          </div>
          {zoomControls}
        </div>
      )}
      {missing.length > 0 && (
        <div className="payvy-viewer-hint">Not found on the page: {missing.map((m) => m.label).join(', ')}</div>
      )}
      <div
        ref={scroller}
        className="payvy-viewer-canvas"
        data-paged={paging ? '' : undefined}
        onTouchStart={(e) => {
          const t = e.touches[0];
          swipe.current = paged && e.touches.length === 1 && t ? { x: t.clientX, y: t.clientY } : null;
        }}
        onTouchEnd={(e) => {
          const from = swipe.current;
          swipe.current = null;
          const t = e.changedTouches[0];
          const el = scroller.current;
          if (!from || !t || !el || el.scrollWidth > el.clientWidth + 1) return;
          const dx = t.clientX - from.x;
          const dy = t.clientY - from.y;
          if (Math.abs(dx) > 60 && Math.abs(dx) > 2 * Math.abs(dy)) go(index + (dx < 0 ? 1 : -1));
        }}
      >
        {loaded.kind === 'loading' && (
          <div className="payvy-preview-loading">
            <div role="status" className="payvy-loading-message">
              <Spinner size="sm" />
              Opening the original document
            </div>
            <div className="payvy-paper-placeholder" aria-hidden="true">
              <span />
              <span />
              <span />
              <hr />
              <span />
              <span />
            </div>
          </div>
        )}
        {loaded.kind === 'error' && (
          <Banner
            status="error"
            title="Couldn’t open the original"
            description={loaded.message}
            endContent={<Button label="Retry preview" variant="secondary" onClick={() => setAttempt((n) => n + 1)} />}
          />
        )}
        {ready && (
          <VStack gap={4} hAlign="center">
            {visible.map((p, i) => (
              <PageView
                key={p.ref.page}
                page={p.page}
                pageNumber={p.ref.page}
                rotation={p.ref.rotation}
                scale={s}
                rects={found[offset + i] ?? []}
                focus={focus ?? null}
              />
            ))}
          </VStack>
        )}
      </div>
    </VStack>
  );
}

/** Zoom out. */
function MinusIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" {...props}>
      <path d="M5 12h14" />
    </svg>
  );
}

/** Zoom in. */
function PlusIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" {...props}>
      <path d="M12 5v14M5 12h14" />
    </svg>
  );
}

function PageView({
  page,
  pageNumber,
  rotation,
  scale,
  rects,
  focus,
}: {
  page: Page;
  pageNumber: number;
  rotation: number;
  scale: number;
  rects: (Rect & { key: string; label: string })[];
  focus: string | null;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    if (!canvas.current) return;
    // Zooming re-renders; pdf.js refuses two renders into one canvas at once.
    const task = page.render(canvas.current, scale, rotation);
    task.done.catch((e: unknown) => {
      if (!(e instanceof Error && e.name === 'RenderingCancelledException')) console.error(e);
    });
    return task.cancel;
  }, [page, scale, rotation]);
  const size = turnedSize(page, rotation);
  const pad = 2;
  return (
    <div
      data-page={pageNumber}
      data-rotation={rotation}
      style={{
        position: 'relative',
        width: size.width * scale,
        height: size.height * scale,
        background: 'white',
        boxShadow: 'var(--shadow-med, 0 1px 4px rgba(0,0,0,.3))',
        flex: 'none',
      }}
    >
      <canvas ref={canvas} style={{ width: '100%', height: '100%', display: 'block' }} />
      {rects.map((r, i) => (
        <div
          key={i}
          data-hl={r.key}
          title={r.label}
          className={focus === r.key ? 'payvy-hl payvy-hl-focus' : 'payvy-hl'}
          style={{
            position: 'absolute',
            left: r.x * scale - pad,
            top: r.y * scale - pad,
            width: r.w * scale + pad * 2,
            height: r.h * scale + pad * 2,
          }}
        />
      ))}
    </div>
  );
}
