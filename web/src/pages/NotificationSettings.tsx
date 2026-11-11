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
interface Settings { enabled: boolean; reminders: boolean; updated_by: string | null; updated_at: string | null }
interface Data { settings: Settings; sender: 'unconfigured' | 'log' | 'email' }
export function NotificationSettings() {
  const [data, setData] = useState<Data | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const toast = useToast();
  const load = useCallback(async () => {
    try { setData((await run<Data>('notification.settings', {})).result); setError(null); }
    catch (e) { setError(e instanceof Error ? e.message : String(e)); }
  }, []);
  useEffect(() => { void load(); }, [load]);
  async function change(patch: Partial<Settings>) {
    if (!data) return;
    setBusy(true);
    try {
      const { enabled, reminders } = { ...data.settings, ...patch };
      setData((await run<Data>('notification.configure', { enabled, reminders })).result);
      toast({ body: 'Notification settings saved' });
    } catch (e) { toast({ body: e instanceof Error ? e.message : String(e), type: 'error' }); await load(); }
    finally { setBusy(false); }
  }
  return <Page title="Notifications" narrow description="Let people know when a bill needs their attention.">
    {error && <Banner status="error" title="Couldn't load notification settings" description={error} />}
    {!data && !error && <Spinner label="Loading notifications" />}
    {data && <VStack gap={4}>
      {data.sender === 'unconfigured' && <Banner status="warning" title="Email isn't configured" description="Ask your server administrator to configure the web address and outgoing email before enabling notifications." />}
      {data.sender === 'log' && <Banner status="warning" title="Development mode: emails are logged" description="No email reaches anyone. Delivery history will show Logged only until an outgoing email sender is configured." />}
      <Card><VStack gap={4}>
        <Switch label="Email people when a bill needs them" value={data.settings.enabled} isDisabled={busy || data.sender === 'unconfigured'} changeAction={enabled => change({ enabled })}
          description="Notify eligible approvers and payment releasers. Turning this on also checks existing unpaid bills. People sign in to review and act; email never approves or pays a bill." />
        <Switch label="Remind people about pending approvals" value={data.settings.reminders} isDisabled={busy} changeAction={reminders => change({ reminders })}
          description="Send the first reminder 48 hours after the initial notice, then once a day, up to three reminders per person and approval step. This measures waiting time, not the invoice due date." />
      </VStack></Card>
      <Text type="supporting">Recipients follow the bill's current step, company roles, and approval rules. Notices stop when the bill moves on or someone loses access. Automatic payment steps don't send a release notice unless they fall back to a person.</Text>
      <Text type="supporting">Open Email notifications on a bill to see its delivery history. Turning notifications off pauses pending mail; a message already being sent may still arrive.</Text>
      {data.settings.updated_by && <Text type="supporting">Last changed by {data.settings.updated_by}{data.settings.updated_at ? ` · ${new Date(data.settings.updated_at).toLocaleString()}` : ''}</Text>}
    </VStack>}
  </Page>;
}
