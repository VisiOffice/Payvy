import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { HStack } from '@astryxdesign/core/HStack';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { find, type Item, type Rect } from './match';

export interface Highlight {
  key: string;
  label: string;
  /** Ways the value may be printed; the first found wins. */
  candidates: string[];
}

interface Page {
  width: number;
  height: number;
  /** Renders at a scale into a canvas; cancel before rendering again. */
  render: (canvas: HTMLCanvasElement, scale: number) => { done: Promise<void>; cancel: () => void };
  items: Item[];
}

/** The file through the server (same origin, signed in), as bytes. */
async function fetchFile(documentId: string): Promise<ArrayBuffer> {
  const res = await fetch(`/api/documents/${documentId}/file`, {
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
      render: (canvas, scale) => {
        const ratio = window.devicePixelRatio || 1;
        const viewport = page.getViewport({ scale: scale * ratio });
        canvas.width = viewport.width;
        canvas.height = viewport.height;
        const task = page.render({ canvas, viewport });
        return { done: task.promise, cancel: () => task.cancel() };
      },
    });
  }
  return pages;
}

type Loaded =
  | { kind: 'loading' }
  | { kind: 'pdf'; pages: Page[] }
  | { kind: 'image'; url: string }
  | { kind: 'error'; message: string };

const ZOOMS = [0.5, 0.67, 0.8, 1, 1.25, 1.5, 2, 3];

/**
 * A document with zoom and highlights of what was read from it. Clicking a
 * field elsewhere sets `focus`, which scrolls to it and pulses it.
 */
export function DocumentViewer({
  documentId,
  contentType,
  highlights = [],
  focus,
}: {
  documentId: string;
  contentType: string;
  highlights?: Highlight[];
  focus?: string | null;
}) {
  const [loaded, setLoaded] = useState<Loaded>({ kind: 'loading' });
  const [scale, setScale] = useState<number | 'fit'>('fit');
  const [fitScale, setFitScale] = useState(1);
  const scroller = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    let url: string | null = null;
    setLoaded({ kind: 'loading' });
    fetchFile(documentId)
      .then(async (data) => {
        if (contentType === 'application/pdf') {
          const pages = await loadPdf(data);
          if (!cancelled) setLoaded({ kind: 'pdf', pages });
        } else {
          url = URL.createObjectURL(new Blob([data], { type: contentType }));
          if (!cancelled) setLoaded({ kind: 'image', url });
        }
      })
      .catch((e) => !cancelled && setLoaded({ kind: 'error', message: e.message }));
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [documentId, contentType]);

  // Fit width follows the panel's width.
  useEffect(() => {
    const el = scroller.current;
    if (!el || loaded.kind !== 'pdf') return;
    const widest = Math.max(...loaded.pages.map((p) => p.width));
    const update = () => setFitScale(Math.max(0.25, (el.clientWidth - 32) / widest));
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, [loaded]);

  const s = scale === 'fit' ? fitScale : scale;

  const zoom = useCallback(
    (dir: 1 | -1) => {
      const current = scale === 'fit' ? fitScale : scale;
      const next = dir > 0 ? ZOOMS.find((z) => z > current + 0.01) : [...ZOOMS].reverse().find((z) => z < current - 0.01);
      if (next) setScale(next);
    },
    [scale, fitScale],
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

  // Highlights per page, found once per document.
  const found = useMemo(() => {
    if (loaded.kind !== 'pdf') return [];
    return loaded.pages.map((p) =>
      highlights.flatMap((h) => find(p.items, h.candidates).map((r) => ({ ...r, key: h.key, label: h.label }))),
    );
  }, [loaded, highlights]);

  // Scroll the focused field into view.
  useEffect(() => {
    if (!focus || !scroller.current) return;
    const el = scroller.current.querySelector<HTMLElement>(`[data-hl="${CSS.escape(focus)}"]`);
    el?.scrollIntoView({ behavior: 'smooth', block: 'center', inline: 'center' });
  }, [focus, found, s]);

  const missing = useMemo(() => {
    if (loaded.kind !== 'pdf') return [];
    const seen = new Set(found.flat().map((r) => r.key));
    return highlights.filter((h) => !seen.has(h.key));
  }, [found, highlights, loaded.kind]);

  return (
    <VStack height="100%" gap={0}>
      <HStack gap={2} vAlign="center" paddingInline={3} paddingBlock={2} style={{ borderBottom: '1px solid var(--color-border)' }}>
        <Button label="Zoom out" size="sm" variant="ghost" onClick={() => zoom(-1)} />
        <Text type="supporting" hasTabularNumbers>
          {Math.round(s * 100)}%
        </Text>
        <Button label="Zoom in" size="sm" variant="ghost" onClick={() => zoom(1)} />
        <Button label="Fit width" size="sm" variant="ghost" onClick={() => setScale('fit')} />
        {loaded.kind === 'pdf' && missing.length > 0 && (
          <Text type="supporting" maxLines={1}>
            Not found on the page: {missing.map((m) => m.label).join(', ')}
          </Text>
        )}
      </HStack>
      <div
        ref={scroller}
        style={{ flex: 1, overflow: 'auto', background: 'var(--color-background-wash, #1a1a1a)', padding: 16 }}
      >
        {loaded.kind === 'loading' && <Spinner label="Opening the document" />}
        {loaded.kind === 'error' && <Banner status="error" title={loaded.message} />}
        {loaded.kind === 'image' && (
          <img
            src={loaded.url}
            alt="The document"
            style={{ width: scale === 'fit' ? '100%' : `${s * 100}%`, display: 'block', margin: '0 auto' }}
          />
        )}
        {loaded.kind === 'pdf' && (
          <VStack gap={4} hAlign="center">
            {loaded.pages.map((p, i) => (
              <PageView key={i} page={p} scale={s} rects={found[i] ?? []} focus={focus ?? null} />
            ))}
          </VStack>
        )}
      </div>
    </VStack>
  );
}

function PageView({
  page,
  scale,
  rects,
  focus,
}: {
  page: Page;
  scale: number;
  rects: (Rect & { key: string; label: string })[];
  focus: string | null;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    if (!canvas.current) return;
    // Zooming re-renders; pdf.js refuses two renders into one canvas at once.
    const task = page.render(canvas.current, scale);
    task.done.catch((e: unknown) => {
      if (!(e instanceof Error && e.name === 'RenderingCancelledException')) console.error(e);
    });
    return task.cancel;
  }, [page, scale]);
  const pad = 2;
  return (
    <div
      style={{
        position: 'relative',
        width: page.width * scale,
        height: page.height * scale,
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
