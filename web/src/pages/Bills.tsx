import { useEffect, useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Heading } from '@astryxdesign/core/Heading';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Table, type TableColumn } from '@astryxdesign/core/Table';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { date, money } from '../money';

interface BillRow extends Record<string, unknown> {
  id: string;
  vendor: string;
  invoice: string;
  due: string;
  amount: string;
  status: string;
}

interface Listed {
  bill: {
    id: string;
    vendor_name: string;
    invoice_number: string;
    due_date: string | null;
    amount_cents: number;
    state: string;
  };
  progress: unknown;
}

/** Open bills' progress is a status object, closed ones have none. */
function statusOf(l: Listed): string {
  const p = l.progress as { status?: string } | string | null;
  const s = typeof p === 'string' ? p : (p?.status ?? l.bill.state);
  return s.replaceAll('_', ' ');
}

const STATUS_VARIANT: Record<string, 'neutral' | 'info' | 'success' | 'warning' | 'error'> = {
  paid: 'success',
  released: 'info',
  'awaiting approval': 'warning',
  'awaiting release': 'warning',
  rejected: 'error',
  void: 'neutral',
};

const columns: TableColumn<BillRow>[] = [
  { key: 'vendor', header: 'Vendor' },
  { key: 'invoice', header: 'Invoice' },
  { key: 'due', header: 'Due' },
  { key: 'amount', header: 'Amount', align: 'end' },
  {
    key: 'status',
    header: 'Status',
    renderCell: (row) => <Badge label={row.status} variant={STATUS_VARIANT[row.status] ?? 'neutral'} />,
  },
];

export function Bills({ mine = false }: { mine?: boolean }) {
  const [rows, setRows] = useState<BillRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setRows(null);
    run<{ bills: Listed[] }>('bill.list', mine ? { mine: true } : { limit: 200 })
      .then((r) =>
        setRows(
          r.result.bills.map((l) => ({
            id: l.bill.id,
            vendor: l.bill.vendor_name,
            invoice: l.bill.invoice_number,
            due: date(l.bill.due_date),
            amount: money(l.bill.amount_cents),
            status: statusOf(l),
          })),
        ),
      )
      .catch((e) => setError(e.message));
  }, [mine]);

  return (
    <VStack gap={4} padding={6}>
      <VStack gap={1}>
        <Heading level={1}>{mine ? 'Waiting on you' : 'All bills'}</Heading>
        <Text type="supporting">
          {mine
            ? 'Bills a step is waiting on you for: approving or releasing payment.'
            : 'Every bill, newest first.'}
        </Text>
      </VStack>
      {error && <Banner status="error" title="Couldn't load bills" description={error} />}
      {!rows && !error && <Spinner label="Loading bills" />}
      {rows && rows.length === 0 && (
        <EmptyState
          title={mine ? 'Nothing is waiting on you' : 'No bills yet'}
          description={
            mine
              ? 'Bills that need your approval or release show up here.'
              : 'Bills arrive by email, from agents, or through the API.'
          }
        />
      )}
      {rows && rows.length > 0 && <Table data={rows} columns={columns} idKey="id" hasHover />}
    </VStack>
  );
}
