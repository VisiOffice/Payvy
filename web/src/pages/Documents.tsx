import { useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Heading } from '@astryxdesign/core/Heading';
import { Link } from '@astryxdesign/core/Link';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Table, type TableColumn } from '@astryxdesign/core/Table';
import { Text } from '@astryxdesign/core/Text';
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

  useEffect(() => {
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

  return (
    <VStack gap={4} padding={6}>
      <VStack gap={1}>
        <Heading level={1}>Documents</Heading>
        <Text type="supporting">
          Invoices that arrived by email or were uploaded. Check what was read from each, then make it a bill.
        </Text>
      </VStack>
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
