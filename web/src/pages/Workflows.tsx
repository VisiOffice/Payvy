// The default workflow, edited here, and the vendors that don't follow it.
// A vendor's own workflow is edited on its page.
import { useCallback, useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Heading } from '@astryxdesign/core/Heading';
import { Link } from '@astryxdesign/core/Link';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Table, type TableColumn } from '@astryxdesign/core/Table';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import type { Current } from '../workflow';
import { Builder, loadPeople } from '../workflows/Builder';
import type { PersonOption } from '../workflows/StepEditor';
import { WorkflowCell, type VendorSummary } from './Vendors';

const columns: TableColumn<VendorSummary>[] = [
  {
    key: 'name',
    header: 'Vendor',
    renderCell: (row) => <Link href={`/vendors/${row.id}#workflow`}>{row.name}</Link>,
  },
  { key: 'own_workflow', header: 'Workflow', renderCell: (row) => <WorkflowCell v={row} /> },
];

export function Workflows() {
  const [base, setBase] = useState<Current | null>(null);
  const [people, setPeople] = useState<PersonOption[] | null>(null);
  const [exceptions, setExceptions] = useState<VendorSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loadBase = useCallback(async () => {
    setBase((await run<Current>('workflow.get', {})).result);
  }, []);

  useEffect(() => {
    Promise.all([
      loadBase(),
      loadPeople().then(setPeople),
      run<{ vendors: VendorSummary[] }>('vendor.list', {}).then((r) =>
        setExceptions(r.result.vendors.filter((v) => v.own_workflow || v.workflow_proposals > 0)),
      ),
    ]).catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [loadBase]);

  return (
    <VStack gap={6} padding={6}>
      <VStack gap={1}>
        <Heading level={1}>Workflows</Heading>
        <Text type="supporting">
          The path each bill takes before it's paid. Every vendor follows the default unless it has its own, which
          you set on the vendor's page. A bill keeps the workflow it arrived under.
        </Text>
      </VStack>
      {error && <Banner status="error" title="Couldn't load workflows" description={error} />}
      {!error && (!base || !people) && <Spinner label="Loading" />}

      {base && people && (
        <VStack gap={3}>
          <Heading level={2}>Default</Heading>
          <Builder current={base} people={people} onSaved={loadBase} />
        </VStack>
      )}

      {exceptions && (
        <VStack gap={3}>
          <Heading level={2}>Vendors with their own</Heading>
          {exceptions.length === 0 ? (
            <Text type="supporting">
              None: every vendor follows the default. To give one its own, open it from Vendors.
            </Text>
          ) : (
            <Table data={exceptions} columns={columns} idKey="id" hasHover />
          )}
        </VStack>
      )}
    </VStack>
  );
}
