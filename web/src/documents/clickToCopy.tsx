// Click-to-copy from the original to the form on document review, as on
// Bill.com: click text on the page to copy it, then click a field to paste it.
// The other order works too: focus a field, then click text on the page. It is
// pointer sugar; the fields still take typing and an ordinary paste.
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type FocusEvent, type ReactNode } from 'react';

/** The fields that take text from the page, by the key their value was read under. */
export type FillKey = 'vendor_name' | 'invoice_number' | 'invoice_date' | 'due_date' | 'total' | 'po_number';

export const FIELD_LABEL: Record<FillKey, string> = {
  vendor_name: 'Vendor',
  invoice_number: 'Invoice number',
  invoice_date: 'Invoice date',
  due_date: 'Due',
  total: 'Amount',
  po_number: 'Memo',
};

/** What filling a field did, in words: `ok` false leaves the copied text to go elsewhere. */
export interface Filled {
  ok: boolean;
  message: string;
}

/** A field's element, found by its key. */
const fieldElement = (key: FillKey) => document.querySelector<HTMLElement>(`[data-copy-field="${key}"]`);

/** Long text cut short for a message. */
export const short = (s: string) => (s.length > 40 ? `${s.slice(0, 39)}…` : s);

function dialogOpen(): boolean {
  return !!document.querySelector('dialog[open], [role="dialog"], [role="alertdialog"], [aria-modal="true"]');
}

export interface ClickToCopy {
  /** For the viewer: text clicked on the page. */
  onCopy: (text: string) => void;
  onHasText: (hasText: boolean) => void;
  /** Props for the wrapper of a field that can be filled. */
  field: (key: FillKey) => {
    'data-copy-field': FillKey;
    'data-flash'?: '';
    className: string;
    onPointerDownCapture: () => void;
    onFocusCapture: () => void;
    onBlurCapture: () => void;
    onClick: () => void;
    onInputCapture: () => void;
  };
  /** A value was chosen in the field (a vendor, a date): it isn't waiting for the page any more. */
  chose: (key: FillKey) => void;
  /** The field that receives the next click on the page, if any: show it a hint. */
  receiving: FillKey | null;
  /** For the form's container: focus moving to something that isn't a field. */
  onFormFocus: (e: FocusEvent) => void;
  /** The copied text and messages, and the live announcement. */
  ui: ReactNode;
}

/**
 * The state of click-to-copy on one document. `editable` is false when the
 * form can't change (filed, locked, or no bill.create): clicking text still
 * copies to the system clipboard, and nothing is pasted into the form.
 */
export function useClickToCopy(editable: boolean, fill: (key: FillKey, text: string, field: HTMLElement | null) => Filled): ClickToCopy {
  const [clip, setClip] = useState<string | null>(null);
  const [target, setTarget] = useState<FillKey | null>(null);
  const [hasText, setHasText] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [flash, setFlash] = useState<FillKey | null>(null);
  const [status, setStatus] = useState('');
  const fillRef = useRef(fill);
  useLayoutEffect(() => {
    fillRef.current = fill;
  });
  // The field a pointer went down in: it pastes on the click, not the focus.
  const pressed = useRef<FillKey | null>(null);
  // Focus leaving a field, to tell a move inside it (its list closing) from arriving.
  const left = useRef<{ key: FillKey; at: number } | null>(null);
  const clipRef = useRef(clip);
  clipRef.current = clip;

  // A message shows for a moment; the copied text stays until it's used or cleared.
  useEffect(() => {
    if (!note) return;
    const t = setTimeout(() => setNote(null), 3000);
    return () => clearTimeout(t);
  }, [note]);
  useEffect(() => {
    if (!flash) return;
    const t = setTimeout(() => setFlash(null), 1300);
    return () => clearTimeout(t);
  }, [flash]);

  const announce = (message: string) => {
    // The same words twice in a row are announced again.
    setStatus((s) => (s === message ? `${message} ` : message));
  };

  const clear = useCallback(() => {
    setClip(null);
    setTarget(null);
    setNote(null);
  }, []);

  const put = (key: FillKey, text: string) => {
    const done = fillRef.current(key, text, fieldElement(key));
    announce(done.message);
    setNote(done.message);
    if (done.ok) setFlash(key);
    return done.ok;
  };

  // Esc lets go of the copied text (and of the field waiting for text), unless a dialog has it.
  const active = !!clip || !!target || !!note;
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || dialogOpen()) return;
      clear();
      announce('Copied text cleared');
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [active, clear]);

  // Nothing waits for a paste once the form can't change.
  useEffect(() => {
    if (!editable) {
      setClip(null);
      setTarget(null);
    }
  }, [editable]);

  const onCopy = (raw: string) => {
    const text = raw.trim();
    if (!text) return;
    // The system clipboard too, where the browser allows it.
    try {
      void navigator.clipboard?.writeText(text).catch(() => {});
    } catch {
      // Not allowed here: the copy stays in the app.
    }
    if (editable && target && !clip) {
      put(target, text);
      setTarget(null);
      return;
    }
    if (!editable) {
      setNote(`Copied “${short(text)}”`);
      announce(`Copied ${text}`);
      return;
    }
    setClip(text);
    setNote(null);
    announce(`Copied ${text}. Click a field to paste it.`);
  };

  const pasteInto = (key: FillKey) => {
    const text = clipRef.current;
    if (!text) return;
    clipRef.current = null;
    // A date that isn't one keeps the copied text for another field.
    if (put(key, text)) setClip(null);
    else clipRef.current = text;
  };

  return {
    onCopy,
    onHasText: setHasText,
    field: (key) => ({
      'data-copy-field': key,
      ...(flash === key ? { 'data-flash': '' as const } : {}),
      className: 'payvy-copy-field',
      onPointerDownCapture: () => {
        pressed.current = key;
      },
      onBlurCapture: () => {
        left.current = { key, at: performance.now() };
      },
      onFocusCapture: () => {
        if (!editable) return;
        // Focus coming back from the field's own list or calendar.
        const moved = left.current?.key === key && performance.now() - left.current.at < 100;
        left.current = null;
        if (moved) return;
        if (clipRef.current) {
          // From the keyboard there's no click to come.
          if (pressed.current !== key) pasteInto(key);
        } else if (hasText) setTarget(key);
      },
      onClick: () => {
        const wasPressed = pressed.current === key;
        pressed.current = null;
        if (editable && wasPressed && clipRef.current) pasteInto(key);
      },
      onInputCapture: () => {
        // Typing in the field: it isn't waiting for text from the page any more.
        // After the input has its value: a render in the middle of the event
        // would put the field's old value back.
        setTimeout(() => setTarget((t) => (t === key ? null : t)));
      },
    }),
    chose: (key) => setTarget((t) => (t === key ? null : t)),
    receiving: editable && hasText && !clip ? target : null,
    onFormFocus: (e) => {
      if (e.target instanceof Element && !e.target.closest('[data-copy-field]')) setTarget(null);
    },
    ui: (
      <>
        <div className="payvy-sr-only" role="status" aria-live="polite" aria-atomic="true">
          {status}
        </div>
        {(clip || note) && (
          <div className="payvy-copy-chip" data-copied={clip ? '' : undefined}>
            <span className="payvy-copy-chip-text">
              {clip ? (
                <>
                  Copied <strong>“{short(clip)}”</strong>
                  <span> · {note ?? 'Click a field to paste'}</span>
                </>
              ) : (
                note
              )}
            </span>
            {clip && (
              <button type="button" aria-label="Clear copied text" title="Clear (Esc)" onClick={clear}>
                Esc
              </button>
            )}
          </div>
        )}
      </>
    ),
  };
}

const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

/** Sets a React-controlled input's value as typing would. */
function typeInto(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
  setter?.call(input, value);
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

/**
 * Opens a Selector with search (the vendor list) and puts `query` in its
 * search, so the person picks; nothing is chosen for them.
 */
export async function searchSelector(field: HTMLElement, query: string): Promise<boolean> {
  const trigger = field.querySelector<HTMLElement>('button');
  if (!trigger) return false;
  // A click on the field opens it by itself; give that a moment first.
  await frame();
  await frame();
  if (trigger.getAttribute('aria-expanded') !== 'true') trigger.click();
  for (let i = 0; i < 30; i++) {
    await frame();
    // The list's search: one list is open at a time.
    const input = document.querySelector<HTMLInputElement>('input[role="combobox"][aria-autocomplete="list"]');
    if (input) {
      typeInto(input, query);
      input.focus();
      return true;
    }
  }
  return false;
}

/**
 * What to search a list of names for, given text from the page: the longest
 * start of it, word by word, that some name contains ("AMERICAN RECLAMATION
 * INC" finds "American Reclamation, Inc." as "AMERICAN RECLAMATION"); the
 * whole text when nothing matches, so the person sees it and can edit.
 */
export function searchFor(text: string, names: string[]): string {
  const lower = names.map((n) => n.toLowerCase());
  const words = text.trim().split(/\s+/);
  for (let n = words.length; n > 0; n--) {
    const q = words.slice(0, n).join(' ').replace(/[,.;:]+$/, '');
    if (q.length >= 2 && lower.some((l) => l.includes(q.toLowerCase()))) return q;
  }
  return text.trim();
}
