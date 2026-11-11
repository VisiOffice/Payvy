// Settings > Purchase orders: the company-wide switch. Off (the default)
// refuses every PO change and never blocks a payment. On, each vendor's
// own settings decide whether its bills need a PO, at once for bills in
// flight. Only a person who is an admin changes it (po.configure).
import { useCallback, useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Card } from '@astryxdesign/core/Card';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Switch } from '@astryxdesign/core/Switch';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { useToast } from '@astryxdesign/core/Toast';
import { Page } from '../Page';
import { run } from '../api/client';

interface Settings { enabled: boolean; updated_by: string | null; updated_at: string | null }

export function PurchaseOrderSettings() {
  const [data, setData] = useState<Settings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const toast = useToast();
  const load = useCallback(async () => {
    try { setData((await run<Settings>('po.settings', {})).result); setError(null); }
    catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  }, []);
  useEffect(() => { void load(); }, [load]);
  async function change(enabled: boolean) {
    setBusy(true);
    try {
      setData((await run<Settings>('po.configure', { enabled })).result);
      toast({ body: enabled ? 'Purchase orders are on' : 'Purchase orders are off' });
    } catch (e) { toast({ body: e instanceof Error ? e.message : String(e), type: 'error' }); await load(); }
    finally { setBusy(false); }
  }
  return <Page title="Purchase orders" narrow description="Match bills against what each vendor is authorized to bill.">
    {error && <Banner status="error" title="Couldn't load purchase order settings" description={error} />}
    {!data && !error && <Spinner label="Loading purchase order settings" />}
    {data && <VStack gap={4}>
      <Card><Switch label="Use purchase orders" value={data.enabled} isDisabled={busy} changeAction={change}
        description="When on, vendors can require a PO, and bills for those vendors can't be paid until they're matched to one or someone permits the exception." /></Card>
      <Text type="supporting">Whether a vendor requires a PO, and how far a bill may run over, is set on each vendor and applies at once, including to bills already waiting. Whoever prepared a bill can't permit its exception, and whoever permits it can't release that bill.</Text>
      <Text type="supporting">Turning purchase orders off hides them and lifts the payment check; nothing is deleted. Rejecting or voiding a bill still returns what it reserved.</Text>
      {data.updated_by && <Text type="supporting">Last changed by {data.updated_by}{data.updated_at ? ` · ${new Date(data.updated_at).toLocaleString()}` : ''}</Text>}
    </VStack>}
  </Page>;
}
