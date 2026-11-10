import { useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Heading } from '@astryxdesign/core/Heading';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { BillsTable, type Listed } from '../bills/BillsTable';

export function Bills({ mine = false }: { mine?: boolean }) {
  const [rows, setRows] = useState<Listed[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setRows(null);
    run<{ bills: Listed[] }>('bill.list', mine ? { mine: true } : { limit: 200 })
      .then((r) => setRows(r.result.bills))
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
      {rows && rows.length > 0 && <BillsTable bills={rows} />}
    </VStack>
  );
}
