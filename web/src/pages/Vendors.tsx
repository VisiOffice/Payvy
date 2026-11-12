import { Allowed } from '../permissions';
import { useEffect, useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Table, pixel, proportional, type TableColumn } from '@astryxdesign/core/Table';
import { Text } from '@astryxdesign/core/Text';
import { useNavigate } from 'react-router';
import { Page } from '../Page';
import { VendorDialog } from './VendorDialog';
import { run } from '../api/client';
import { money } from '../money';

/** A row of vendor.list. */
export interface VendorSummary extends Record<string, unknown> {
  id: string;
  name: string;
  open_bills: number;
  open_cents: number;
  own_workflow: boolean;
  workflow_proposals: number;
}

/** "Its own" or "The default", with any proposals waiting. */
export function WorkflowCell({ v }: { v: VendorSummary }) {
  return (
    <HStack gap={2} vAlign="center">
      {v.own_workflow ? <Text>Its own</Text> : <Text type="supporting">The default</Text>}
      {v.workflow_proposals > 0 && (
        <Badge
          variant="warning"
          label={`${v.workflow_proposals} proposed change${v.workflow_proposals === 1 ? '' : 's'}`}
        />
      )}
    </HStack>
  );
}

const columns: TableColumn<VendorSummary>[] = [
  {
    key: 'name',
    header: 'Vendor',
    width: proportional(3),
    renderCell: (row) => <Link href={`/vendors/${row.id}`}>{row.name}</Link>,
  },
  {
    key: 'open_bills',
    header: 'Open bills',
    width: pixel(120),
    align: 'end',
    renderCell: (row) => (row.open_bills ? String(row.open_bills) : '—'),
  },
  {
    key: 'open_cents',
    header: 'Open amount',
    width: pixel(150),
    align: 'end',
    renderCell: (row) => (row.open_bills ? money(row.open_cents) : '—'),
  },
  { key: 'own_workflow', header: 'Workflow', width: proportional(2), renderCell: (row) => <WorkflowCell v={row} /> },
];

export function Vendors() {
  const [vendors, setVendors] = useState<VendorSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const navigate = useNavigate();

  useEffect(() => {
    run<{ vendors: VendorSummary[] }>('vendor.list', {})
      .then((r) => setVendors(r.result.vendors))
      .catch((e) => setError(e.message));
  }, []);

  return (
    <Page
      wide
      title="Vendors"
      description="Who you pay, what's open with each, and how their bills are approved."
      actions={vendors && <Allowed command="vendor.create"><Button label="Add vendor" variant="primary" onClick={() => setAdding(true)} /></Allowed>}
    >
      {error && <Banner status="error" title="Couldn't load vendors" description={error} />}
      {!vendors && !error && <Spinner label="Loading vendors" />}
      {vendors && vendors.length === 0 && (
        <EmptyState title="No vendors yet" description="Vendors are added as their bills come in, or add one now to set it up ahead of time." />
      )}
      {vendors && vendors.length > 0 && <Table data={vendors} columns={columns} idKey="id" hasHover />}
      {adding && <VendorDialog onClose={() => setAdding(false)} onSaved={(v) => navigate(`/vendors/${v.id}`)} />}
    </Page>
  );
}
