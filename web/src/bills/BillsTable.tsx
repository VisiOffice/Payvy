// A table of bills, for All bills and a vendor's page.
import { Badge } from '@astryxdesign/core/Badge';
import { Link } from '@astryxdesign/core/Link';
import { Table, pixel, proportional, type TableColumn } from '@astryxdesign/core/Table';
import { Text } from '@astryxdesign/core/Text';
import { DueText } from '../Due';
import { date, money } from '../money';
import { billStatus } from '../status';

interface BillRow extends Record<string, unknown> {
  id: string;
  vendor: string;
  invoice: string;
  due: string;
  dueIso: string | null;
  amount: string;
  /** awaiting_release, paid, … */
  status: string;
  sent: string | null;
  revised: boolean;
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
  /** How many copies of this invoice have arrived (2+ shows “Sent N×”). */
  received_count?: number;
  /** A revised copy's amount while it awaits a person's decision. */
  pending_revision_amount_cents?: number | null;
}

/** Open bills' progress is a status object, closed ones have none. */
export function statusOf(l: Listed): string {
  const p = l.progress as { status?: string } | string | null;
  const s = typeof p === 'string' ? p : (p?.status ?? l.bill.state);
  return s;
}


const COLUMNS: TableColumn<BillRow>[] = [
  { key: 'vendor', header: 'Vendor', width: proportional(3), renderCell: (row) => <Link href={`/bills/${row.id}`}>{row.vendor}</Link> },
  {
    key: 'invoice',
    width: proportional(2),
    header: 'Invoice',
    renderCell: (row) => (
      <>
        <Link href={`/bills/${row.id}`}>{row.invoice}</Link>
        {row.sent && <> <Badge label={row.sent} variant="info" /></>}
        {row.revised && <> <Badge label="Revised" variant="warning" /></>}
      </>
    ),
  },
  {
    key: 'due',
    header: 'Due',
    width: pixel(200),
    renderCell: (row) =>
      row.due ? (
        <>
          {row.due} {isOpen(row.status) && <DueText iso={row.dueIso} />}
        </>
      ) : (
        <Text type="supporting">Not set</Text>
      ),
  },
  {
    key: 'amount',
    header: 'Amount',
    width: pixel(140),
    align: 'end',
    renderCell: (row) => (
      <Text weight="semibold" hasTabularNumbers>
        {row.amount}
      </Text>
    ),
  },
  {
    key: 'status',
    header: 'Status',
    width: pixel(170),
    renderCell: (row) => {
      const s = billStatus(row.status);
      return <Badge label={s.label} variant={s.variant} />;
    },
  },
];


/** Paid, released, rejected and void bills are past caring when they were due. */
function isOpen(status: string): boolean {
  return !['paid', 'released', 'rejected', 'void'].includes(status);
}

export function rows(bills: Listed[]): BillRow[] {
  return bills.map((l) => ({
    id: l.bill.id,
    vendor: l.bill.vendor_name,
    invoice: l.bill.invoice_number,
    due: date(l.bill.due_date),
    dueIso: l.bill.due_date,
    amount: money(l.bill.amount_cents),
    status: statusOf(l),
    sent: l.received_count && l.received_count > 1 ? `Sent ${l.received_count}×` : null,
    revised: l.pending_revision_amount_cents != null,
  }));
}

/** Without `showVendor` (on a vendor's own page) the invoice number links to the bill. */
export function BillsTable({ bills, showVendor = true }: { bills: Listed[]; showVendor?: boolean }) {
  const columns = showVendor ? COLUMNS : COLUMNS.filter((c) => c.key !== 'vendor');
  return <div className="payvy-table"><Table data={rows(bills)} columns={columns} idKey="id" hasHover /></div>;
}
