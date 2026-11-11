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
  const [v2,setV2] = useState(false);
  const [error,setError] = useState('');
  useEffect(() => { run<{enabled:boolean;extraction_v2_enabled:boolean}>('invoice_review.settings',{}).then(r => {setEnabled(r.result.enabled);setV2(r.result.extraction_v2_enabled);}).catch(e => setError(String(e))); },[]);
  return <Card><VStack gap={3}>
    <Text weight="semibold">Invoice line review · pilot</Text>
    <Text>Review and correct invoice lines before creating a bill. Discounts stay in the breakdown, and unresolved totals need review.</Text>
    <Text type="supporting">Turning this off stops new enrollment. Previously saved reviews remain required. The richer reader is a separate opt-in. Documents already read with it retain their source checks when it is turned off.</Text>
    {error && <Banner status="error" title={error} />}
    {enabled !== null && <Button label={enabled ? 'Turn off new line reviews' : 'Enable invoice line review'} variant="secondary" isDisabled={!can('invoice_review.configure')} clickAction={async () => {
      const r = await run<{enabled:boolean;extraction_v2_enabled:boolean}>('invoice_review.configure',{enabled:!enabled}); setEnabled(r.result.enabled); setV2(r.result.extraction_v2_enabled);
    }} />}
    {enabled && <>
      <Text weight="semibold">Richer invoice reader · experimental</Text>
      <Text type="supporting">Capture quantities, rates, item codes and printed tax with page citations. Source text and arithmetic checks highlight uncertainty; they do not prove the invoice is correct. Up to 24 selected pages per reading.</Text>
      <Button label={v2 ? 'Turn off richer reader' : 'Enable richer reader'} variant="secondary" isDisabled={!can('invoice_review.configure')} clickAction={async () => {
        const r = await run<{extraction_v2_enabled:boolean}>('invoice_review.configure',{enabled:true,extraction_v2_enabled:!v2}); setV2(r.result.extraction_v2_enabled);
      }} />
    </>}
  </VStack></Card>;
}
