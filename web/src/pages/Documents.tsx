import { useCallback, useEffect, useState } from 'react';
import { useNavigate } from 'react-router';
import { Banner } from '@astryxdesign/core/Banner';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { FileInput } from '@astryxdesign/core/FileInput';
import { Heading } from '@astryxdesign/core/Heading';
import { Link } from '@astryxdesign/core/Link';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Table, type TableColumn } from '@astryxdesign/core/Table';
import { Text } from '@astryxdesign/core/Text';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import type { Doc } from '../documents/types';
import { date } from '../money';

type Status = Doc['status'];

interface Row extends Record<string, unknown> {
  id: string;
  filename: string;
  source: string;
  received: string;
  note: string;
}

const SOURCE: Record<string, string> = { email: 'Email', upload: 'Uploaded' };

/** What document.upload takes; the server checks the bytes, this only steers the picker. */
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

const columns: TableColumn<Row>[] = [
  {
    key: 'filename',
    header: 'Document',
    renderCell: (row) => <Link href={`/documents/${row.id}`}>{row.filename}</Link>,
  },
  { key: 'source', header: 'From' },
  { key: 'received', header: 'Received' },
  { key: 'note', header: '' },
];

/** Files waiting to become bills, and those already filed or set aside. */
export function Documents() {
  const [status, setStatus] = useState<Status>('inbox');
  const [rows, setRows] = useState<Row[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [files, setFiles] = useState<File[]>([]);
  const [uploading, setUploading] = useState<string | null>(null);
  const navigate = useNavigate();
  const toast = useToast();

  const load = useCallback(() => {
    setRows(null);
    run<{ documents: Doc[] }>('document.list', { status, limit: 200 })
      .then((r) =>
        setRows(
          r.result.documents.map((d) => ({
            id: d.id,
            filename: d.filename,
            source: SOURCE[d.source] ?? d.source,
            received: date(d.received_at),
            note: d.trashed_reason ?? '',
          })),
        ),
      )
      .catch((e) => setError(e.message));
  }, [status]);
  useEffect(load, [load]);

  /**
   * Adds each file to the inbox. One file is read straight away and opened
   * for review; several are left for the background reader (or "Read it").
   */
  async function upload(picked: File | File[] | null) {
    const list = picked ? (Array.isArray(picked) ? picked : [picked]) : [];
    if (list.length === 0) return;
    const added: string[] = [];
    try {
      for (const file of list) {
        setUploading(`Uploading ${file.name}…`);
        const r = await run<{ id: string }>('document.upload', {
          filename: file.name,
          content_base64: await base64(file),
        });
        added.push(r.result.id);
      }
      if (added.length === 1) {
        setUploading('Reading it…');
        // A failed read is recorded on the document and shown on its page.
        await run('document.extract', { document_id: added[0]! }).catch(() => undefined);
        navigate(`/documents/${added[0]}`);
        return;
      }
      toast({ body: `Added ${added.length} documents. They're read in the background.` });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      toast({
        body: added.length ? `Added ${added.length}, then stopped: ${msg}` : msg,
        type: 'error',
      });
    } finally {
      setUploading(null);
      setFiles([]);
    }
    if (status === 'inbox') load();
    else setStatus('inbox');
  }

  return (
    <VStack gap={4} padding={6}>
      <VStack gap={1}>
        <Heading level={1}>Documents</Heading>
        <Text type="supporting">
          Invoices that arrived by email or were uploaded. Check what was read from each, then make it a bill.
        </Text>
      </VStack>
      <FileInput
        label="Add invoices"
        isLabelHidden
        mode="dropzone"
        isMultiple
        accept={ACCEPT}
        maxSize={MAX_BYTES}
        placeholder="Drop invoices here, or choose files (PDF or image, up to 25 MB)"
        value={files}
        onChange={(f) => setFiles(f ? (Array.isArray(f) ? f : [f]) : [])}
        changeAction={upload}
        isLoading={!!uploading}
        isDisabled={!!uploading}
        description={uploading ?? undefined}
      />
      <SegmentedControl label="Show" value={status} onChange={(v) => setStatus(v as Status)}>
        <SegmentedControlItem value="inbox" label="To review" />
        <SegmentedControlItem value="filed" label="Filed" />
        <SegmentedControlItem value="trashed" label="Not bills" />
      </SegmentedControl>
      {error && <Banner status="error" title="Couldn't load documents" description={error} />}
      {!rows && !error && <Spinner label="Loading documents" />}
      {rows && rows.length === 0 && (
        <EmptyState
          title={status === 'inbox' ? 'Nothing to review' : 'None yet'}
          description={
            status === 'inbox' ? 'Invoices emailed to your Payvy address or uploaded show up here.' : undefined
          }
        />
      )}
      {rows && rows.length > 0 && <Table data={rows} columns={columns} idKey="id" hasHover />}
    </VStack>
  );
}
