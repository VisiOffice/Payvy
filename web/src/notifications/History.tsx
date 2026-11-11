import { useCallback, useEffect, useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { HStack } from '@astryxdesign/core/HStack';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
interface Notice { id: string; email: string; action: string; step_id: string; reminder: number; status: string; created_at: string; finished_at: string | null; attempts: number; reason: string | null; next_attempt_at: string }
const LABEL: Record<string, string> = { queued: 'Queued', sending: 'Sending', sent: 'Sent to provider', logged: 'Logged only', failed: 'Failed', unknown: 'Delivery uncertain', cancelled: 'No longer needed' };
const REASON: Record<string, string> = { rate_limited: 'The email provider is busy; a retry is scheduled.', rate_limit_exhausted: 'The email provider kept refusing requests; retries stopped.', provider_refused: 'The email provider refused this message. Ask your administrator to check the sender and recipient.', provider_uncertain: 'The provider did not confirm the outcome. This message will not be resent automatically.', interrupted_send: 'Sending was interrupted. This message will not be resent automatically.', no_longer_actionable: 'The person no longer needs this notice, or approval reminders were switched off.' };
export function NotificationHistory({ billId }: { billId: string }) {
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<Notice[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(async () => {
    try { setRows((await run<{ notifications: Notice[] }>('notification.list', { bill_id: billId })).result.notifications); setError(null); }
    catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  }, [billId]);
  useEffect(() => { if (open) void load(); }, [open, load]);
  return <details onToggle={e => setOpen(e.currentTarget.open)}>
    <summary style={{ cursor: 'pointer', fontWeight: 600 }}>Email notifications</summary>
    {open && <VStack gap={3} paddingBlock={3}>
      <HStack hAlign="between" gap={2}><Text type="supporting">Sent means the email provider accepted it, not that it reached an inbox.</Text><Button label="Refresh notifications" size="sm" variant="ghost" clickAction={load} /></HStack>
      {error && <Banner status="error" title="Couldn't load notifications" description={error} />}
      {!rows && !error && <Spinner label="Loading notification history" />}
      {rows?.length === 0 && <Text type="supporting">No notifications yet. A company admin can enable them in Settings → Notifications.</Text>}
      {rows?.map(n => <VStack key={n.id} gap={1}>
        <HStack gap={2} wrap="wrap"><Text weight="semibold">{n.reminder ? `Approval reminder ${n.reminder}` : n.action === 'approve' ? 'Approval needed' : 'Ready for payment release'}</Text><Badge label={LABEL[n.status] ?? n.status} /></HStack>
        <Text type="supporting">{n.email} · {n.step_id} · {new Date(n.finished_at ?? n.created_at).toLocaleString()}</Text>
        {n.reason && <Text type="supporting">{REASON[n.reason] ?? 'Delivery needs attention.'}</Text>}
      </VStack>)}
    </VStack>}
  </details>;
}
