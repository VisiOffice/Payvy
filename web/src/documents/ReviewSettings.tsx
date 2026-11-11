import { useEffect, useState } from 'react';
import { Card } from '@astryxdesign/core/Card';
import { Text } from '@astryxdesign/core/Text';
import { Button } from '@astryxdesign/core/Button';
import { Banner } from '@astryxdesign/core/Banner';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { useCan } from '../permissions';

export function ReviewSettings() {
  const can = useCan();
  const [enabled,setEnabled] = useState<boolean | null>(null);
  const [error,setError] = useState('');
  useEffect(() => { run<{enabled:boolean}>('invoice_review.settings',{}).then(r => setEnabled(r.result.enabled)).catch(e => setError(String(e))); },[]);
  return <Card><VStack gap={3}>
    <Text weight="semibold">Invoice line review · pilot</Text>
    <Text>Review and correct invoice lines before creating a bill. Discounts stay in the breakdown, and unresolved totals need review.</Text>
    <Text type="supporting">Turning this off stops new enrollment. Previously saved reviews remain required. The pilot uses the existing reader; quantity extraction and source verification are not included yet.</Text>
    {error && <Banner status="error" title={error} />}
    {enabled !== null && <Button label={enabled ? 'Turn off new line reviews' : 'Enable invoice line review'} variant="secondary" isDisabled={!can('invoice_review.configure')} clickAction={async () => {
      const r = await run<{enabled:boolean}>('invoice_review.configure',{enabled:!enabled}); setEnabled(r.result.enabled);
    }} />}
  </VStack></Card>;
}
