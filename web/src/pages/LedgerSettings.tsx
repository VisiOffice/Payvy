// Settings > Ledger: connect VisiBooks and say how it posts. The API key
// is write-only: sealed by the server at connect and never shown again.
// Payvy never moves money and never owns the books; it posts what the
// bank already did (or is about to do, at approval) to the company's own
// ledger, per company, by an Admin (ledger.connect / ledger.configure).
import { useCallback, useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { useToast } from '@astryxdesign/core/Toast';
import { Page } from '../Page';
import { preview, run } from '../api/client';

interface LedgerSettings {
  connected: boolean;
  provider?: string;
  origin?: string | null;
  entity?: string;
  post_when?: 'approval' | 'payment';
  bank_account_map?: Record<string, string>;
  status?: string;
}

export function LedgerSettings() {
  const [data, setData] = useState<LedgerSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [origin, setOrigin] = useState('');
  const [entity, setEntity] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [postWhen, setPostWhen] = useState<'approval' | 'payment'>('approval');
  const [mapText, setMapText] = useState('');
  const toast = useToast();

  const load = useCallback(async () => {
    try {
      const s = (await run<LedgerSettings>('ledger.settings', {})).result;
      setData(s);
      if (s.post_when) setPostWhen(s.post_when);
      setMapText(JSON.stringify(s.bank_account_map ?? {}, null, 2));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);
  useEffect(() => { void load(); }, [load]);

  async function connect() {
    setBusy(true);
    try {
      const input = { origin: origin.trim(), entity: entity.trim(), api_key: apiKey, post_when: postWhen };
      const p = await preview<LedgerSettings>('ledger.connect', input);
      const r = await run<LedgerSettings>('ledger.connect', input, p.confirm_token);
      setData(r.result);
      setApiKey('');
      toast({ body: 'VisiBooks connected' });
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    } finally { setBusy(false); }
  }

  async function save() {
    setBusy(true);
    try {
      let map: Record<string, string>;
      try { map = JSON.parse(mapText) as Record<string, string>; }
      catch { toast({ body: 'The bank account map must be JSON, like {"bank-connection-id": "ledger-account-id"}', type: 'error' }); return; }
      const r = await run<LedgerSettings>('ledger.configure', { post_when: postWhen, bank_account_map: map });
      setData(r.result);
      toast({ body: 'Ledger settings saved' });
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
      await load();
    } finally { setBusy(false); }
  }

  return (
    <Page title="Ledger" narrow description="Post bills and payments to the company's own books (VisiBooks). Payvy posts what the bank does; it never moves money.">
      {error && <Banner status="error" title="Couldn't load ledger settings" description={error} />}
      {!data && !error && <Spinner label="Loading ledger settings" />}
      {data && !data.connected && (
        <Card>
          <VStack gap={3}>
            <Text weight="semibold">Connect VisiBooks</Text>
            <TextInput label="Origin" value={origin} onChange={setOrigin} placeholder="https://books.example.com" />
            <TextInput label="Entity ID" value={entity} onChange={setEntity} />
            <TextInput label="API key" type="password" value={apiKey} onChange={setApiKey} />
            <Text type="supporting">The key is sealed on the server and never shown again. Reconnecting with a new key replaces it on the same connection.</Text>
            <SegmentedControl label="Post bills" value={postWhen} onChange={(v) => setPostWhen(v as 'approval' | 'payment')}>
              <SegmentedControlItem value="approval" label="When approved" />
              <SegmentedControlItem value="payment" label="When paid" />
            </SegmentedControl>
            <Button label="Connect" variant="primary" clickAction={connect} isDisabled={busy || !origin.trim() || !entity.trim() || !apiKey} />
          </VStack>
        </Card>
      )}
      {data?.connected && (
        <VStack gap={4}>
          <Card>
            <VStack gap={3}>
              <Text weight="semibold">VisiBooks · entity {data.entity}</Text>
              <Text type="supporting">{data.origin} · {data.status}</Text>
              <SegmentedControl label="Post bills" value={postWhen} onChange={(v) => setPostWhen(v as 'approval' | 'payment')}>
                <SegmentedControlItem value="approval" label="When approved" />
                <SegmentedControlItem value="payment" label="When paid" />
              </SegmentedControl>
              <Text type="supporting">
                When approved: a bill posts as soon as its approval completes, and its payment posts when the bank
                sends it. When paid: bill and payment post together at payment. A bill paid outside Payvy posts its
                bill and leaves the payment leg out (it has no Payvy bank account to draw on).
              </Text>
            </VStack>
          </Card>
          <Card>
            <VStack gap={3}>
              <Text weight="semibold">Bank accounts in the ledger</Text>
              <Text type="supporting">
                Which ledger bank account each Payvy bank connection pays from, as JSON
                (Payvy bank connection id → ledger account id). A payment with no mapped account is not posted.
              </Text>
              <TextInput label="Bank account map (JSON)" value={mapText} onChange={setMapText} />
              <Button label="Save ledger settings" variant="primary" clickAction={save} isDisabled={busy} />
            </VStack>
          </Card>
        </VStack>
      )}
    </Page>
  );
}
