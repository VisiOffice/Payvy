// A table of bills, for All bills and a vendor's page.
import { Badge } from '@astryxdesign/core/Badge';
import { Link } from '@astryxdesign/core/Link';
import { Table, type TableColumn } from '@astryxdesign/core/Table';
import { date, money } from '../money';

interface BillRow extends Record<string, unknown> {
  id: string;
  vendor: string;
  invoice: string;
  due: string;
  amount: string;
  status: string;
}

export interface Listed {
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

const COLUMNS: TableColumn<BillRow>[] = [
  { key: 'vendor', header: 'Vendor', renderCell: (row) => <Link href={`/bills/${row.id}`}>{row.vendor}</Link> },
  { key: 'invoice', header: 'Invoice', renderCell: (row) => <Link href={`/bills/${row.id}`}>{row.invoice}</Link> },
  { key: 'due', header: 'Due' },
  { key: 'amount', header: 'Amount', align: 'end' },
  {
    key: 'status',
    header: 'Status',
    renderCell: (row) => <Badge label={row.status} variant={STATUS_VARIANT[row.status] ?? 'neutral'} />,
  },
];


export function rows(bills: Listed[]): BillRow[] {
  return bills.map((l) => ({
    id: l.bill.id,
    vendor: l.bill.vendor_name,
    invoice: l.bill.invoice_number,
    due: date(l.bill.due_date),
    amount: money(l.bill.amount_cents),
    status: statusOf(l),
  }));
}

/** Without `showVendor` (on a vendor's own page) the invoice number links to the bill. */
export function BillsTable({ bills, showVendor = true }: { bills: Listed[]; showVendor?: boolean }) {
  const columns = showVendor
    ? COLUMNS.map((c) => (c.key === 'invoice' ? { key: c.key, header: c.header } : c))
    : COLUMNS.filter((c) => c.key !== 'vendor');
  return <Table data={rows(bills)} columns={columns} idKey="id" hasHover />;
}
