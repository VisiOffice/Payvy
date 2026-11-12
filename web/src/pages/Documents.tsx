import { Allowed, useCan } from '../permissions';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Kbd } from '@astryxdesign/core/Kbd';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Spinner } from '@astryxdesign/core/Spinner';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { Page } from '../Page';
import { run } from '../api/client';
import type { Doc } from '../documents/types';
import { InboxBoard, InboxTable, typing, useInboxView, type Row } from '../documents/InboxViews';
import { readWithin } from '../documents/Loading';

type Status = Doc['status'];
const STATUS_KEY: Record<string, Status> = { r: 'inbox', f: 'filed', n: 'trashed' };
const LABEL: Record<Status, string> = { inbox: 'To review', filed: 'Filed', trashed: 'Not bills', split: 'Split' };
const ACCEPT = 'application/pdf,image/png,image/jpeg,image/gif,image/webp,image/tiff';
const MAX_BYTES = 25 * 1024 * 1024;
function isInvoiceFile(file: File): boolean {
  const type = file.type.toLowerCase();
  return type === 'application/pdf' || type.startsWith('image/') || /\.(pdf|png|jpe?g|gif|webp|tiff?)$/i.test(file.name);
}
/** Files this page will take, and a sentence for each one it will not. */
function chosen(list: File[]): { ok: File[]; problems: string[] } {
  const ok: File[] = [];
  const problems: string[] = [];
  for (const file of list) {
    if (file.size > MAX_BYTES) problems.push(`${file.name} is larger than 25 MB`);
    else if (!isInvoiceFile(file)) problems.push(`${file.name} isn't a PDF or a photo`);
    else ok.push(file);
  }
  return { ok, problems };
}
/**
 * True while files are dragged over the window and this page will take them.
 * The listeners stay up either way: a file drag is always cancelled, so the
 * browser never opens it in the tab. When uploads aren't allowed the cursor
 * says so and nothing is taken. A counter covers entering a child, which
 * fires leave and enter together.
 */
function useFileDrag(onDrop: (files: File[]) => void, enabled: boolean): boolean {
  const [over, setOver] = useState(false);
  const depth = useRef(0);
  const drop = useRef(onDrop);
  const allow = useRef(enabled);
  drop.current = onDrop;
  allow.current = enabled;
  useEffect(() => {
    if (!enabled) { depth.current = 0; setOver(false); }
  }, [enabled]);
  useEffect(() => {
    const hasFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types ?? []).includes('Files');
    const enter = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = allow.current ? 'copy' : 'none';
      if (!allow.current) return;
      depth.current += 1;
      setOver(true);
    };
    const overEv = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = allow.current ? 'copy' : 'none';
    };
    const leave = (e: DragEvent) => {
      if (!hasFiles(e) || !allow.current) return;
      depth.current = Math.max(0, depth.current - 1);
      if (depth.current === 0) setOver(false);
    };
    const dropped = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      depth.current = 0;
      setOver(false);
      if (!allow.current) return;
      const files = Array.from(e.dataTransfer?.files ?? []);
      if (files.length) drop.current(files);
    };
    const end = () => { depth.current = 0; setOver(false); };
    window.addEventListener('dragenter', enter);
    window.addEventListener('dragover', overEv);
    window.addEventListener('dragleave', leave);
    window.addEventListener('drop', dropped);
    window.addEventListener('dragend', end);
    return () => {
      window.removeEventListener('dragenter', enter);
      window.removeEventListener('dragover', overEv);
      window.removeEventListener('dragleave', leave);
      window.removeEventListener('drop', dropped);
      window.removeEventListener('dragend', end);
    };
  }, []);
  return over && enabled;
}

/** The file picker. The drop target is the page itself. */
function AddInvoices({ busy, onFiles }: { busy: boolean; onFiles: (files: File[]) => void }) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <Button label="Add invoices" tooltip="PDFs, scans, or photos, up to 25 MB each" onClick={() => input.current?.click()} isDisabled={busy} />
      <input ref={input} type="file" className="payvy-sr-only" accept={ACCEPT} multiple tabIndex={-1} aria-hidden="true" onChange={e => { onFiles(Array.from(e.target.files ?? [])); e.target.value = ''; }} />
    </>
  );
}
function base64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).slice(String(r.result).indexOf(',') + 1));
    r.onerror = () => reject(r.error ?? new Error(`Couldn't read ${file.name}`));
    r.readAsDataURL(file);
  });
}
export const fileSize = (bytes: number) => bytes < 1024 * 1024 ? `${Math.max(1, Math.round(bytes / 1024))} KB` : `${(bytes / (1024 * 1024)).toFixed(1)} MB`;

export function Documents() {
  const [view, setView] = useInboxView();
  const [status, setStatus] = useState<Status>('inbox');
  const show = (s: Status) => { setStatus(s); setSearch(''); };
  // R, F and N pick To review, Filed and Not bills; those are lists, so the table shows.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (typing(e)) return;
      const s = STATUS_KEY[e.key.toLowerCase()];
      if (!s) return;
      e.preventDefault();
      show(s);
      setView('table');
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });
  const [filed, setFiled] = useState<Row[] | null>(null);
  const [docs, setDocs] = useState<Row[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [uploading, setUploading] = useState<{ title: string; detail: string } | null>(null);
  const canUpload = useCan()('document.upload');
  const accept = useRef<(files: File[]) => void>(() => {});
  // Listeners stay mounted. A drop is taken only while this person may upload and nothing is already uploading.
  const dragging = useFileDrag((files) => accept.current(files), canUpload && !uploading);
  const request = useRef(0);
  const navigate = useNavigate();
  const toast = useToast();
  const load = useCallback(() => {
    const current = ++request.current;
    setDocs(null); setError(null);
    readWithin(run<{ documents: Row[] }>('document.list', { status, limit: 200 }))
      .then(r => { if (current === request.current) setDocs(r.result.documents); })
      .catch(e => { if (current === request.current) setError(e.message); });
  }, [status]);
  useEffect(() => { load(); return () => { request.current++; }; }, [load]);
  // The board's last column: what became bills, latest first.
  const board = view === 'board';
  const loadFiled = useCallback(() => {
    if (!board) return;
    run<{ documents: Row[] }>('document.list', { status: 'filed', limit: 200 })
      .then(r => setFiled(r.result.documents.slice(-12).reverse()))
      .catch(() => setFiled([]));
  }, [board]);
  useEffect(() => { loadFiled(); }, [loadFiled]);
  // The board is To review; switching to it shows that.
  useEffect(() => { if (board && status !== 'inbox') { setStatus('inbox'); setSearch(''); } }, [board, status]);
  const refresh = () => { load(); loadFiled(); };

  async function upload(list: File[]) {
    if (!list.length) return;
    const added: string[] = [];
    try {
      for (const [index, file] of list.entries()) {
        setUploading({ title: `Uploading ${index + 1} of ${list.length}`, detail: file.name });
        const r = await run<{ id: string }>('document.upload', { filename: file.name, content_base64: await base64(file) });
        added.push(r.result.id);
      }
      if (added.length === 1) {
        setUploading({ title: 'Invoice uploaded. Reading the details…', detail: 'We’ll open it for review when the reading finishes.' });
        await run('document.extract', { document_id: added[0]! }).catch(() => undefined);
        navigate(`/inbox/${added[0]}`);
        return;
      }
      toast({ body: `Added ${added.length} documents. They're read in the background.` });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast({ body: added.length ? `Added ${added.length}, then stopped: ${msg}` : msg, type: 'error' });
    } finally { setUploading(null); }
    if (status === 'inbox') load(); else setStatus('inbox');
  }
  accept.current = (list) => {
    const { ok, problems } = chosen(list);
    if (problems.length) toast({ body: `${problems.join('. ')}.`, type: 'error' });
    if (ok.length) void upload(ok);
  };
  /** Puts a split document's pages back where they came from. */
  async function undoSplit(d: Row) {
    try {
      await run('document.unsplit', { document_id: d.id });
      toast({ body: `Pages put back in ${d.split_from?.filename ?? 'the document they came from'}` });
      refresh();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }
  const q = search.trim().toLocaleLowerCase();
  const visible = docs?.filter(d => !q || [d.filename, d.reading?.vendor.name, d.reading?.vendor.printed, ...(d.reading?.invoice_numbers ?? [])]
    .some(v => v?.toLocaleLowerCase().includes(q)));
  return <Page title="Inbox" eyebrow="Invoice intake" description="Every invoice that arrives, with what was read from it. Review the details, then create a bill.">
    <div className="payvy-documents-toolbar">
      <div className="payvy-documents-switches">
        {!board && <div className="payvy-view-switch">
          <SegmentedControl label="Show" value={status} onChange={v => show(v as Status)}>
            <SegmentedControlItem value="inbox" label="To review" aria-keyshortcuts="R" icon={<Kbd keys="r" aria-hidden />} />
            <SegmentedControlItem value="filed" label="Filed" aria-keyshortcuts="F" icon={<Kbd keys="f" aria-hidden />} />
            <SegmentedControlItem value="trashed" label="Not bills" aria-keyshortcuts="N" icon={<Kbd keys="n" aria-hidden />} />
          </SegmentedControl>
        </div>}
        <div className="payvy-view-switch">
          <SegmentedControl label="View" value={view} onChange={v => setView(v as 'table' | 'board')}>
            <SegmentedControlItem value="table" label="Table" aria-keyshortcuts="T" icon={<Kbd keys="t" aria-hidden />} />
            <SegmentedControlItem value="board" label="Board" aria-keyshortcuts="B" icon={<Kbd keys="b" aria-hidden />} />
          </SegmentedControl>
        </div>
      </div>
      <div className="payvy-documents-actions">
        <div className="payvy-documents-search"><TextInput label="Search documents" isLabelHidden placeholder="Search vendors, invoices, files…" value={search} onChange={setSearch} /></div>
        <Allowed command="document.upload"><AddInvoices busy={!!uploading} onFiles={(files) => accept.current(files)} /></Allowed>
        <Button label="Refresh" variant="ghost" onClick={refresh} />
      </div>
    </div>
    {dragging && <div className="payvy-drop-overlay" role="status"><strong>Drop to add invoices</strong><p>PDFs, scans, or photos · up to 25 MB each</p></div>}
    {uploading && <div role="status" className="payvy-upload-progress"><Spinner size="sm" /><div><strong>{uploading.title}</strong><p>{uploading.detail}</p></div></div>}
    {error && <Banner status="error" title="Couldn't load documents" description={error} endContent={<Button label="Try again" variant="secondary" onClick={load} />} />}
    {!docs && !error && <div className="payvy-document-list-loading" aria-busy="true"><div role="status" className="payvy-loading-message"><Spinner size="sm" />Loading documents</div><div aria-hidden="true">{[0, 1, 2].map(i => <div className="payvy-document-row-placeholder" key={i}><span className="payvy-file-glyph" /><span className="payvy-skeleton" /><span className="payvy-skeleton" /></div>)}</div></div>}
    {visible && board && <InboxBoard docs={visible} filed={filed} undoSplit={undoSplit} />}
    {visible && !board && <section className="payvy-document-list" aria-label={LABEL[status]}>
      <div className="payvy-document-list-heading"><h2>{LABEL[status]}</h2><span>{visible.length} {visible.length === 1 ? 'document' : 'documents'}{docs!.length === 200 ? ' · latest 200 loaded' : ''}</span></div>
      {visible.length === 0 ? <EmptyState title={search ? 'No matching documents' : status === 'inbox' ? 'You’re all caught up' : 'Nothing here yet'} description={search ? 'Try another vendor, invoice number or filename.' : status === 'filed' ? 'Documents appear here after you create a bill.' : status === 'trashed' ? 'Documents you set aside stay here.' : 'Upload an invoice or email one to your email-in address.'} /> :
        <InboxTable docs={visible} undoSplit={undoSplit} />}
    </section>}
  </Page>;
}
