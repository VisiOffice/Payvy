import { useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';

export interface RampHealthState {
  status: 'unknown' | 'connected' | 'retrying' | 'paused' | 'needs_reconnection';
  message: string;
  access_expired: boolean;
  checked_at: string | null;
  next_check_at: string | null;
  retry_at: string | null;
  can_check: boolean;
}
const LABEL: Record<RampHealthState['status'], string> = {
  unknown: 'Checking connection', connected: 'Connected', retrying: 'Waiting to retry',
  paused: 'Checks paused', needs_reconnection: 'Needs reconnection',
};
export function RampHealth({ connectionId, health, onChanged }: { connectionId: string; health?: RampHealthState; onChanged: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!health) return null;
  return <VStack gap={2}>
    <HStack gap={2} wrap="wrap">
      <Badge label={LABEL[health.status]} variant={health.status === 'connected' ? 'success' : 'warning'} />
      <Badge label="Setup only" variant="warning" />
    </HStack>
    <Text>{health.message}</Text>
    {health.checked_at && <Text type="supporting">Last checked {new Date(health.checked_at).toLocaleString()}</Text>}
    {health.status === 'retrying' && health.next_check_at && <Text type="supporting">Next automatic check {new Date(health.next_check_at).toLocaleString()}</Text>}
    {health.status === 'paused' && health.retry_at && <Text type="supporting">You can check again after {new Date(health.retry_at).toLocaleString()}</Text>}
    {health.status === 'needs_reconnection'
      ? <Link href="#ramp-connect">Reconnect Ramp</Link>
      : <Button label={busy ? 'Checking Ramp…' : 'Check Ramp connection'} variant="secondary" isDisabled={busy || !health.can_check} onClick={async () => {
        setBusy(true); setError(null);
        try { await run('bank.renew', { connection_id: connectionId }); onChanged(); }
        catch (e) { setError(e instanceof Error ? e.message : String(e)); onChanged(); }
        finally { setBusy(false); }
      }} />}
    {error && <Banner status="error" title={error} />}
  </VStack>;
}
