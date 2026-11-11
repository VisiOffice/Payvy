import { Allowed } from '../permissions';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { FileInput } from '@astryxdesign/core/FileInput';
import { Link } from '@astryxdesign/core/Link';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Spinner } from '@astryxdesign/core/Spinner';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { Page } from '../Page';
import { run } from '../api/client';
import type { Doc, SplitFrom } from '../documents/types';
import { readWithin } from '../documents/Loading';
import { date } from '../money';

type Status = Doc['status'];
/** A row of document.list: a split document says where it came from. */
type Row = Doc & { split_from: SplitFrom | null };
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
  const [status, setStatus] = useState<Status>('inbox');
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
      load();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }
  const visible = docs?.filter(d => d.filename.toLocaleLowerCase().includes(search.toLocaleLowerCase()));
  return <Page title="Documents" eyebrow="Invoice intake" description="A place for every invoice. Review the details, then create a bill.">
    <Allowed command="document.upload"><section className="payvy-document-upload" aria-label="Add invoices">
      <div className="payvy-upload-intro"><span className="payvy-file-glyph" aria-hidden="true">↑</span><div><h2>Add your invoices</h2><p>PDFs, scans, or photos · up to 25 MB each</p></div></div>
      <FileInput label="Add invoices" isLabelHidden mode="dropzone" isMultiple accept={ACCEPT} maxSize={MAX_BYTES}
        placeholder="Drop invoices here, or choose files" value={files}
        onChange={f => setFiles(f ? (Array.isArray(f) ? f : [f]) : [])} changeAction={upload} isDisabled={!!uploading} />
      {uploading && <div role="status" className="payvy-upload-progress"><Spinner size="sm" /><div><strong>{uploading.title}</strong><p>{uploading.detail}</p></div></div>}
    </section></Allowed>
    <div className="payvy-documents-toolbar">
      <SegmentedControl label="Show" value={status} onChange={v => { setStatus(v as Status); setSearch(''); }}>
        <SegmentedControlItem value="inbox" label="To review" /><SegmentedControlItem value="filed" label="Filed" /><SegmentedControlItem value="trashed" label="Not bills" />
      </SegmentedControl>
      <div className="payvy-documents-search"><TextInput label="Search documents" isLabelHidden placeholder="Search filenames…" value={search} onChange={setSearch} /><Button label="Refresh" variant="ghost" onClick={load} /></div>
    </div>
    {error && <Banner status="error" title="Couldn't load documents" description={error} endContent={<Button label="Try again" variant="secondary" onClick={load} />} />}
    {!docs && !error && <div className="payvy-document-list-loading" aria-busy="true"><div role="status" className="payvy-loading-message"><Spinner size="sm" />Loading documents</div><div aria-hidden="true">{[0, 1, 2].map(i => <div className="payvy-document-row-placeholder" key={i}><span className="payvy-file-glyph" /><span className="payvy-skeleton" /><span className="payvy-skeleton" /></div>)}</div></div>}
    {visible && <section className="payvy-document-list" aria-label={LABEL[status]}>
      <div className="payvy-document-list-heading"><h2>{LABEL[status]}</h2><span>{visible.length} {visible.length === 1 ? 'document' : 'documents'}{docs!.length === 200 ? ' · latest 200 loaded' : ''}</span></div>
      {visible.length === 0 ? <EmptyState title={search ? 'No matching documents' : status === 'inbox' ? 'You’re all caught up' : 'Nothing here yet'} description={search ? 'Try another filename.' : status === 'filed' ? 'Documents appear here after you create a bill.' : status === 'trashed' ? 'Documents you set aside stay here.' : 'Upload an invoice or forward one to your bill inbox.'} /> :
        <table className="payvy-documents-table"><thead><tr><th>Document</th><th>Source</th><th>Received</th><th><span className="payvy-sr-only">Status</span></th></tr></thead><tbody>
          {visible.map(d => <tr key={d.id}><td><div className="payvy-document-name"><span className="payvy-file-glyph" aria-hidden="true">{d.content_type === 'application/pdf' ? 'PDF' : 'IMG'}</span><div><Link href={`/documents/${d.id}`}>{d.filename}</Link><p>{fileSize(d.size_bytes)}{d.trashed_reason ? ` · ${d.trashed_reason}` : ''}</p>
            {d.split_from && (
              <p className="payvy-split-from">
                <span>Split from {d.split_from.filename ?? 'another document'}</span>
                {d.status === 'inbox' && (
                  <Allowed command="document.unsplit"><Button
                    label={`Undo split of ${d.filename}`}
                    size="sm"
                    variant="ghost"
                    clickAction={() => undoSplit(d)}
                  >
                    Undo
                  </Button></Allowed>
                )}
              </p>
            )}
          </div></div></td><td>{d.source === 'email' ? 'Email' : 'Upload'}</td><td>{date(d.received_at)}</td><td><span className="payvy-document-status">{LABEL[d.status]}</span></td></tr>)}
        </tbody></table>}
    </section>}
  </Page>;
}
