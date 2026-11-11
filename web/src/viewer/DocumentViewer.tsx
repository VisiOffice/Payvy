import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
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

/**
 * A document with zoom and highlights of what was read from it. Clicking a
 * field elsewhere sets `focus`, which scrolls to it and pulses it. With an
 * `arrangement`, pages show in its order and rotation, and hidden ones are
 * left out; the original file is unchanged.
 */
export function DocumentViewer({
  documentId,
  contentType,
  highlights = [],
  focus,
  arrangement,
}: {
  documentId: string;
  contentType: string;
  highlights?: Highlight[];
  focus?: string | null;
  arrangement?: PageRef[] | null;
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

  // Fit width follows the panel's width.
  useEffect(() => {
    const el = scroller.current;
    if (!el || shown.length === 0) return;
    const sizes = shown.map((s) => turnedSize(s.page, s.ref.rotation));
    const widest = Math.max(...sizes.map((p) => p.width));
    const tallest = Math.max(...sizes.map((p) => p.height));
    const update = () => {
      if (!el.clientWidth || !el.clientHeight) return;
      const width = Math.max(0.25, (el.clientWidth - 48) / widest);
      setFitScale(width);
      setPageScale(Math.max(0.25, Math.min(width, (el.clientHeight - 48) / tallest)));
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, [shown]);

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

  // Scroll the focused field into view.
  useEffect(() => {
    if (!focus || !scroller.current) return;
    const el = scroller.current.querySelector<HTMLElement>(`[data-hl="${CSS.escape(focus)}"]`);
    el?.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'center' });
  }, [focus, found, s]);

  const hasText = ready && loaded.pages.some((p) => p.items.length > 0);
  const missing = useMemo(() => {
    if (!hasText) return [];
    const seen = new Set(found.flat().map((r) => r.key));
    return highlights.filter((h) => !seen.has(h.key));
  }, [found, highlights, hasText]);

  const hiddenCount = ready ? Math.max(0, loaded.pages.length - shown.length) : 0;

  return (
    <VStack height="100%" gap={0} style={{ minHeight: 0 }}>
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
        <div className="payvy-viewer-controls">
          <div className="payvy-viewer-control-group">
            <Button label="Zoom out" size="sm" variant="ghost" isDisabled={!ready || s <= ZOOMS[0]!} onClick={() => zoom(-1)} />
            <span className="payvy-viewer-scale">{ready ? `${Math.round(s * 100)}%` : '—'}</span>
            <Button
              label="Zoom in"
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
      </div>
      {missing.length > 0 && (
        <div className="payvy-viewer-hint">Not found on the page: {missing.map((m) => m.label).join(', ')}</div>
      )}
      <div ref={scroller} className="payvy-viewer-canvas">
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
            {shown.map((p, i) => (
              <PageView
                key={p.ref.page}
                page={p.page}
                pageNumber={p.ref.page}
                rotation={p.ref.rotation}
                scale={s}
                rects={found[i] ?? []}
                focus={focus ?? null}
              />
            ))}
          </VStack>
        )}
      </div>
    </VStack>
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
