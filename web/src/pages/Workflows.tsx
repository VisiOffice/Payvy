import { useEffect, useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Table, type TableColumn } from '@astryxdesign/core/Table';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { Chart } from '../workflows/Chart';
import type { Current } from '../workflow';

interface VendorRow extends Record<string, unknown> {
  id: string;
  name: string;
  own_workflow: boolean;
  workflow_proposals: number;
}

const columns: TableColumn<VendorRow>[] = [
  {
    key: 'name',
    header: 'Vendor',
    renderCell: (row) => <Link href={`/workflows/${row.id}`}>{row.name}</Link>,
  },
  {
    key: 'own_workflow',
    header: 'Workflow',
    renderCell: (row) => (row.own_workflow ? <Text>Its own</Text> : <Text type="supporting">The default</Text>),
  },
  {
    key: 'workflow_proposals',
    header: '',
    renderCell: (row) =>
      row.workflow_proposals > 0 ? (
        <Badge
          variant="warning"
          label={`${row.workflow_proposals} proposed change${row.workflow_proposals === 1 ? '' : 's'}`}
        />
      ) : null,
  },
];

/** The default workflow and which vendors have their own. */
export function Workflows() {
  const [base, setBase] = useState<Current | null>(null);
  const [vendors, setVendors] = useState<VendorRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([
      run<Current>('workflow.get', {}),
      run<{ vendors: VendorRow[] }>('vendor.list', {}),
    ])
      .then(([w, v]) => {
        setBase(w.result);
        setVendors(v.result.vendors);
      })
      .catch((e) => setError(e.message));
  }, []);

  return (
    <VStack gap={6} padding={6}>
      <VStack gap={1}>
        <Heading level={1}>Workflows</Heading>
        <Text type="supporting">
          The path each bill takes before it's paid. A vendor follows the default unless it has its own; a bill
          keeps the workflow it arrived under.
        </Text>
      </VStack>
      {error && <Banner status="error" title="Couldn't load workflows" description={error} />}
      {!error && (!base || !vendors) && <Spinner label="Loading" />}

      {base && (
        <Card>
          <VStack gap={4}>
            <HStack gap={2} hAlign="between" vAlign="center">
              <VStack gap={0}>
                <HStack gap={2} vAlign="center">
                  <Heading level={2}>Default</Heading>
                  {base.proposals.length > 0 && (
                    <Badge variant="warning" label={`${base.proposals.length} proposed`} />
                  )}
                </HStack>
                <Text type="supporting">
                  {base.source === 'builtin'
                    ? "Payvy's starting point: a person releases every bill. Change it to fit how you pay."
                    : 'For every vendor without its own workflow.'}
                </Text>
              </VStack>
              <Button label="Edit" href="/workflows/default" />
            </HStack>
            <Chart steps={base.workflow.steps} label="The default workflow" />
          </VStack>
        </Card>
      )}

      {vendors && (
        <VStack gap={3}>
          <Heading level={2}>Vendors</Heading>
          {vendors.length === 0 ? (
            <EmptyState title="No vendors yet" description="Vendors are added as their bills come in." />
          ) : (
            <Table data={vendors} columns={columns} idKey="id" hasHover />
          )}
        </VStack>
      )}
    </VStack>
  );
}
