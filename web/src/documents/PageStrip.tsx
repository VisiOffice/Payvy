import { useCan } from '../permissions';
import { useEffect, useRef, useState, type SVGProps } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { CheckboxInput } from '@astryxdesign/core/CheckboxInput';
import { Icon } from '@astryxdesign/core/Icon';
import { useToast } from '@astryxdesign/core/Toast';
import { run } from '../api/client';
import { useDocumentPages, type Page } from '../viewer/DocumentViewer';
import { move, update, type PageRef, type Pages } from './pages';
import type { DocumentDetail } from './types';

/** A quarter turn clockwise. */
function RotateIcon(props: SVGProps<SVGSVGElement>) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      {...props}
    >
      <path d="M20 11a8 8 0 1 0-2.34 5.66" />
      <path d="M20 4v7h-7" />
    </svg>
  );
}

const THUMB_WIDTH = 88;

/** A small picture of one page, drawn once per rotation. */
function Thumbnail({ page, rotation, label }: { page: Page; rotation: number; label: string }) {
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    const canvas = document.createElement('canvas');
    const turned = rotation === 90 || rotation === 270;
    const task = page.render(canvas, THUMB_WIDTH / (turned ? page.height : page.width), rotation);
    task.done
      .then(() => !cancelled && setSrc(canvas.toDataURL('image/png')))
      .catch(() => {});
    return () => {
      cancelled = true;
      task.cancel();
    };
  }, [page, rotation]);
  return src ? <img src={src} alt={label} /> : <span className="payvy-page-thumb-blank" role="img" aria-label={label} />;
}

/**
 * The document's pages as thumbnails, in the order they are shown. Move a
 * page earlier or later (or drag it), turn it, hide or restore it; each
 * change saves straight away as a new version through `document.arrange`.
 * With `selected`, each visible page has a checkbox ("Attach page"), with
 * All and None, for creating a bill from some of the pages.
 */
export function PageStrip({
  documentId,
  contentType,
  pages,
  onChange,
  onStale,
  selected,
  onSelect,
}: {
  documentId: string;
  contentType: string;
  pages: Pages;
  /** The document after a change, as document.arrange returns it. */
  onChange: (detail: DocumentDetail) => void;
  /** Someone else changed the pages first: load them again. */
  onStale: () => void;
  /** Pages ticked for a bill (the original's numbers); leave out when pages can't be picked. */
  selected?: ReadonlySet<number>;
  onSelect?: (next: Set<number>) => void;
}) {
  const can = useCan();
  const loaded = useDocumentPages(documentId, contentType);
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const [dragging, setDragging] = useState<number | null>(null);
  const strip = useRef<HTMLOListElement>(null);
  const refocus = useRef<string | null>(null);

  // Keep the keyboard on the control just used, wherever its page moved.
  useEffect(() => {
    const label = refocus.current;
    if (!label || saving || !strip.current) return;
    refocus.current = null;
    // A button that is now disabled (a page moved to the end) still takes focus.
    const next = strip.current.querySelector<HTMLElement>(`button[aria-label="${CSS.escape(label)}"]`);
    (next ?? strip.current.querySelector<HTMLElement>('li'))?.focus();
  }, [pages, saving]);

  const arrangement = pages.arrangement;
  if (!arrangement) {
    return (
      <section className="payvy-pages" aria-label="Pages">
        <div className="payvy-pages-head">
          <strong>Pages</strong>
          <span>This file can't be read page by page, so its pages can't be arranged.</span>
        </div>
      </section>
    );
  }

  const locked = pages.locked;
  const hidden = arrangement.filter((p) => p.hidden).length;
  const visible = arrangement.length - hidden;

  async function save(next: PageRef[], focusLabel: string) {
    setSaving(true);
    refocus.current = focusLabel;
    try {
      const r = await run<DocumentDetail>('document.arrange', {
        document_id: documentId,
        expected_version: pages.version,
        pages: next,
      });
      onChange(r.result);
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
      onStale();
    } finally {
      setSaving(false);
    }
  }

  const disabled = locked || saving || !can('document.arrange');
  const picking = !!selected && !!onSelect;
  const visiblePages = arrangement.filter((p) => !p.hidden).map((p) => p.page);
  const ticked = visiblePages.filter((p) => selected?.has(p)).length;
  function tick(page: number, on: boolean) {
    const next = new Set(selected);
    if (on) next.add(page);
    else next.delete(page);
    onSelect?.(next);
  }

  return (
    <section className="payvy-pages" aria-label="Pages" aria-busy={saving}>
      <div className="payvy-pages-head">
        <strong>Pages</strong>
        <span>
          {arrangement.length} {arrangement.length === 1 ? 'page' : 'pages'}
          {hidden > 0 && ` · ${hidden} hidden`}
        </span>
        {locked && pages.locked_reason && (
          <span className="payvy-pages-locked" role="note">
            {pages.locked_reason}
          </span>
        )}
        {picking && visiblePages.length > 1 && (
          <span className="payvy-pages-pick">
            <span role="status">
              {ticked} of {visiblePages.length} selected
            </span>
            <Button
              label="Select all pages"
              size="sm"
              variant="ghost"
              isDisabled={ticked === visiblePages.length}
              onClick={() => onSelect?.(new Set(visiblePages))}
            >
              All
            </Button>
            <Button
              label="Select no pages"
              size="sm"
              variant="ghost"
              isDisabled={ticked === 0}
              onClick={() => onSelect?.(new Set())}
            >
              None
            </Button>
          </span>
        )}
      </div>
      <ol ref={strip} className="payvy-pages-strip">
        {arrangement.map((p, i) => {
          const page = loaded.kind === 'ready' ? loaded.pages[p.page - 1] : undefined;
          const label = `Page ${p.page}`;
          return (
            <li
              key={p.page}
              className="payvy-page-thumb"
              data-hidden={p.hidden || undefined}
              data-dragging={dragging === i || undefined}
              aria-label={`${label}${p.hidden ? ', hidden' : ''}${p.rotation ? `, rotated ${p.rotation}°` : ''}`}
              tabIndex={-1}
              draggable={!disabled}
              onDragStart={(e) => {
                setDragging(i);
                e.dataTransfer.effectAllowed = 'move';
              }}
              onDragEnd={() => setDragging(null)}
              onDragOver={(e) => {
                if (dragging !== null && dragging !== i) e.preventDefault();
              }}
              onDrop={(e) => {
                e.preventDefault();
                if (dragging === null || dragging === i) return;
                const from = dragging;
                setDragging(null);
                void save(move(arrangement, from, i), `Move page ${arrangement[from]!.page} earlier`);
              }}
            >
              <div className="payvy-page-thumb-picture">
                {page ? (
                  <Thumbnail page={page} rotation={p.rotation} label={label} />
                ) : (
                  <span className="payvy-page-thumb-blank" role="img" aria-label={label} />
                )}
              </div>
              {picking && !p.hidden && visiblePages.length > 1 ? (
                <div className="payvy-page-thumb-pick">
                  <CheckboxInput
                    label={`Attach page ${p.page}`}
                    isLabelHidden
                    size="sm"
                    value={selected.has(p.page)}
                    onChange={(on) => tick(p.page, on)}
                  />
                  <span className="payvy-page-thumb-number">{label}</span>
                </div>
              ) : (
                <span className="payvy-page-thumb-number">
                  {p.hidden ? `${label} · hidden` : label}
                </span>
              )}
              <div className="payvy-page-thumb-actions">
                <Button
                  label={`Move page ${p.page} earlier`}
                  tooltip="Move earlier"
                  icon={<Icon icon="chevronLeft" />}
                  isIconOnly
                  size="sm"
                  variant="ghost"
                  isDisabled={disabled || i === 0}
                  clickAction={() => save(move(arrangement, i, i - 1), `Move page ${p.page} earlier`)}
                />
                <Button
                  label={`Move page ${p.page} later`}
                  tooltip="Move later"
                  icon={<Icon icon="chevronRight" />}
                  isIconOnly
                  size="sm"
                  variant="ghost"
                  isDisabled={disabled || i === arrangement.length - 1}
                  clickAction={() => save(move(arrangement, i, i + 1), `Move page ${p.page} later`)}
                />
                <Button
                  label={`Rotate page ${p.page}`}
                  tooltip="Rotate clockwise"
                  icon={<Icon icon={RotateIcon} />}
                  isIconOnly
                  size="sm"
                  variant="ghost"
                  isDisabled={disabled}
                  clickAction={() =>
                    save(update(arrangement, i, { rotation: (p.rotation + 90) % 360 }), `Rotate page ${p.page}`)
                  }
                />
                {p.hidden ? (
                  <Button
                    label={`Restore page ${p.page}`}
                    size="sm"
                    variant="secondary"
                    isDisabled={disabled}
                    clickAction={() => save(update(arrangement, i, { hidden: false }), `Hide page ${p.page}`)}
                  >
                    Restore
                  </Button>
                ) : (
                  <Button
                    label={`Hide page ${p.page}`}
                    tooltip={visible === 1 ? 'One page must stay visible' : 'Hide'}
                    icon={<Icon icon="eyeSlash" />}
                    isIconOnly
                    size="sm"
                    variant="ghost"
                    isDisabled={disabled || visible === 1}
                    clickAction={() => save(update(arrangement, i, { hidden: true }), `Restore page ${p.page}`)}
                  />
                )}
              </div>
            </li>
          );
        })}
      </ol>
    </section>
  );
}
