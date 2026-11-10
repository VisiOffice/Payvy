// Settings > Bill inbox: the address bills are emailed or forwarded to.
// Creating or replacing it is a person's act (email.new_address is
// person_only), so it lives here.
import { useCallback, useEffect, useState } from 'react';
import { AlertDialog } from '@astryxdesign/core/AlertDialog';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { HStack } from '@astryxdesign/core/HStack';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { ApiError, run } from '../api/client';
import { date } from '../money';
import { Page } from '../Page';

interface Address {
  address: string | null;
  created_at?: string;
  created_by?: string;
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function EmailSettings() {
  const toast = useToast();
  const [current, setCurrent] = useState<Address | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [replacing, setReplacing] = useState(false);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setCurrent((await run<Address>('email.address', {})).result);
    } catch (e) {
      if (e instanceof ApiError && e.code === 'not_configured') setUnavailable(true);
      else setError(message(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  async function create() {
    setBusy(true);
    try {
      const r = await run<{ address: string; retired: string | null }>('email.new_address', {});
      toast({ body: r.result.retired ? `${r.result.retired} no longer works` : 'Bill inbox created' });
      setReplacing(false);
      await load();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally {
      setBusy(false);
    }
  }

  async function copy(address: string) {
    try {
      await navigator.clipboard.writeText(address);
      toast({ body: 'Copied' });
    } catch {
      toast({ body: "Couldn't copy. Select the address and copy it instead.", type: 'error' });
    }
  }

  return (
    <Page
      title="Bill inbox"
      narrow
      description="Vendors send bills to this address, or you forward them. Attachments land in Documents, are read, and wait for you to review."
    >
      {error && <Banner status="error" title="Couldn't load the bill inbox" description={error} />}
      {unavailable && (
        <Banner
          status="info"
          title="Email intake isn't set up on this server"
          description="Whoever runs Payvy sets PAYVY_INBOUND_DOMAIN and PAYVY_INBOUND_SECRET. Until then, upload bills on Documents."
        />
      )}
      {!current && !error && !unavailable && <Spinner label="Loading" />}
      {current && (
        <Card>
          {current.address ? (
            <VStack gap={4}>
              <HStack gap={3} vAlign="center" wrap="wrap">
                <code className="payvy-inbox-address" aria-label="Bill inbox address">
                  {current.address}
                </code>
                <Button label="Copy" variant="secondary" size="sm" onClick={() => copy(current.address!)} />
              </HStack>
              <Text type="supporting">
                Add a tag to tell sources apart, e.g. {current.address.replace('@', '+fuel@')}. Anyone with the address can
                send to it, so give it only to vendors and the people who forward bills.
              </Text>
              <MetadataList>
                <MetadataListItem label="Created">
                  {current.created_at ? date(current.created_at) : '—'}
                  {current.created_by ? ` by ${current.created_by}` : ''}
                </MetadataListItem>
              </MetadataList>
              <HStack>
                <Button label="Replace address…" variant="ghost" onClick={() => setReplacing(true)} />
              </HStack>
            </VStack>
          ) : (
            <VStack gap={3}>
              <Text>No bill inbox yet.</Text>
              <HStack>
                <Button label="Create address" variant="primary" isDisabled={busy} clickAction={create} />
              </HStack>
            </VStack>
          )}
        </Card>
      )}
      <AlertDialog
        isOpen={replacing}
        onOpenChange={setReplacing}
        title="Replace the bill inbox address?"
        description={`${current?.address ?? 'The current address'} stops working at once, and mail sent to it is lost. Use this if it's getting spam, then give the new address to your vendors and update any forwarding rules.`}
        actionLabel="Replace"
        isActionLoading={busy}
        onAction={create}
      />
    </Page>
  );
}
