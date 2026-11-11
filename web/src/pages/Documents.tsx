import { Allowed } from '../permissions';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { FileInput } from '@astryxdesign/core/FileInput';
import { Kbd } from '@astryxdesign/core/Kbd';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Spinner } from '@astryxdesign/core/Spinner';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { Page } from '../Page';
import { run } from '../api/client';
import type { Doc } from '../documents/types';
import { InboxBoard, InboxTable, useInboxView, type Row } from '../documents/InboxViews';
import { readWithin } from '../documents/Loading';

type Status = Doc['status'];
const LABEL: Record<Status, string> = { inbox: 'To review', filed: 'Filed', trashed: 'Not bills', split: 'Split' };
const ACCEPT = 'application/pdf,image/png,image/jpeg,image/gif,image/webp,image/tiff';
const MAX_BYTES = 25 * 1024 * 1024;
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
  const [filed, setFiled] = useState<Row[] | null>(null);
  const [docs, setDocs] = useState<Row[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [files, setFiles] = useState<File[]>([]);
  const [uploading, setUploading] = useState<{ title: string; detail: string } | null>(null);
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

  async function upload(picked: File | File[] | null) {
    const list = picked ? (Array.isArray(picked) ? picked : [picked]) : [];
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
        navigate(`/documents/${added[0]}`);
        return;
      }
      toast({ body: `Added ${added.length} documents. They're read in the background.` });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast({ body: added.length ? `Added ${added.length}, then stopped: ${msg}` : msg, type: 'error' });
    } finally { setUploading(null); setFiles([]); }
    if (status === 'inbox') load(); else setStatus('inbox');
  }
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
    <Allowed command="document.upload"><section className="payvy-document-upload" aria-label="Add invoices">
      <div className="payvy-upload-intro"><span className="payvy-file-glyph" aria-hidden="true">↑</span><div><h2>Add your invoices</h2><p>PDFs, scans, or photos · up to 25 MB each</p></div></div>
      <FileInput label="Add invoices" isLabelHidden mode="dropzone" isMultiple accept={ACCEPT} maxSize={MAX_BYTES}
        placeholder="Drop invoices here, or choose files" value={files}
        onChange={f => setFiles(f ? (Array.isArray(f) ? f : [f]) : [])} changeAction={upload} isDisabled={!!uploading} />
      {uploading && <div role="status" className="payvy-upload-progress"><Spinner size="sm" /><div><strong>{uploading.title}</strong><p>{uploading.detail}</p></div></div>}
    </section></Allowed>
    <div className="payvy-documents-toolbar">
      <div className="payvy-documents-switches">
        {!board && <SegmentedControl label="Show" value={status} onChange={v => { setStatus(v as Status); setSearch(''); }}>
          <SegmentedControlItem value="inbox" label="To review" /><SegmentedControlItem value="filed" label="Filed" /><SegmentedControlItem value="trashed" label="Not bills" />
        </SegmentedControl>}
        <div className="payvy-view-switch">
          <SegmentedControl label="View" value={view} onChange={v => setView(v as 'table' | 'board')}>
            <SegmentedControlItem value="table" label="Table" /><SegmentedControlItem value="board" label="Board" />
          </SegmentedControl>
          <span className="payvy-view-keys" aria-hidden="true"><Kbd keys="t" /><Kbd keys="b" /></span>
        </div>
      </div>
      <div className="payvy-documents-search"><TextInput label="Search documents" isLabelHidden placeholder="Search vendors, invoices, files…" value={search} onChange={setSearch} /><Button label="Refresh" variant="ghost" onClick={refresh} /></div>
    </div>
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
