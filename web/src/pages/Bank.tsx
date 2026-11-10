// Bank settings: the bank bills are paid from (Mercury) and the banks Payvy
// only reads (Rho). Connecting and disconnecting are a person's acts
// (bank.connect and bank.disconnect are person_only), so they live here.
import { useCallback, useEffect, useState } from 'react';
import { AlertDialog } from '@astryxdesign/core/AlertDialog';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { CheckboxInput } from '@astryxdesign/core/CheckboxInput';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { RadioList, RadioListItem } from '@astryxdesign/core/RadioList';
import { Selector } from '@astryxdesign/core/Selector';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { date, money } from '../money';
import { Page } from '../Page';

interface Connection {
  id: string;
  provider: 'mercury' | 'rho';
  environment: 'production' | 'sandbox';
  mode: 'request' | 'direct' | 'read_only';
  account_name: string;
  created_at: string;
  created_by: string;
  webhook_id?: string | null;
}

interface BankState {
  connection: Connection | null;
  connections: (Connection & { capability: 'pay' | 'read_only' })[];
}

interface Account {
  id: string;
  name: string;
  available_cents: number;
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function Bank() {
  const [bank, setBank] = useState<BankState | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    run<BankState>('bank.get', {})
      .then((r) => {
        setBank(r.result);
        setError(null);
      })
      .catch((e) => setError(message(e)));
  }, []);
  useEffect(load, [load]);

  const rho = bank?.connections.find((c) => c.provider === 'rho' && c.capability === 'read_only') ?? null;

  return (
    <Page
      narrow
      title="Bank"
      description="The bank bills are paid from, and the banks Payvy reads. Payvy keeps each token encrypted and never holds money or account numbers."
    >
      {error && <Banner status="error" title="Couldn't load your bank connections" description={error} />}
      {!bank && !error && <Spinner label="Loading" />}
      {bank && (
        <>
          <PayingBank connection={bank.connection} onChange={load} />
          <ReadingBank connection={rho} onChange={load} />
        </>
      )}
    </Page>
  );
}

function Connected({ c }: { c: Connection }) {
  return (
    <Text type="supporting">
      Connected by {c.created_by} on {date(c.created_at)}
    </Text>
  );
}

function Disconnect({ c, consequence, onDone }: { c: Connection; consequence: string; onDone: () => void }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const toast = useToast();
  const name = c.provider === 'rho' ? 'Rho' : 'Mercury';
  return (
    <>
      <Button label="Disconnect" variant="ghost" onClick={() => setOpen(true)} />
      <AlertDialog
        isOpen={open}
        onOpenChange={setOpen}
        title={`Disconnect ${name}?`}
        description={consequence}
        actionLabel="Disconnect"
        isActionLoading={busy}
        onAction={async () => {
          setBusy(true);
          try {
            await run('bank.disconnect', c.provider === 'rho' ? { provider: 'rho' } : {});
            toast({ body: `Disconnected ${name}` });
            setOpen(false);
            onDone();
          } catch (e) {
            toast({ body: message(e), type: 'error' });
          } finally {
            setBusy(false);
          }
        }}
      />
    </>
  );
}

// ── Pays bills ──────────────────────────────────────────────────────────────

const MODE: Record<string, string> = {
  request: 'Each payment waits in Mercury for an admin to approve it',
  direct: 'Payments are sent as soon as a bill is released',
};

function PayingBank({ connection, onChange }: { connection: Connection | null; onChange: () => void }) {
  const [replacing, setReplacing] = useState(false);
  const c = connection;
  return (
    <VStack gap={3}>
      <VStack gap={1}>
        <Heading level={2}>Pays bills</Heading>
        <Text type="supporting">When a person releases a bill, Payvy asks this bank to pay the vendor.</Text>
      </VStack>
      <Card>
        {c && !replacing ? (
          <VStack gap={4}>
            <HStack gap={2} vAlign="center" wrap="wrap">
              <Text weight="semibold">Mercury</Text>
              {c.environment === 'sandbox' && <Badge label="Sandbox" variant="warning" />}
            </HStack>
            <MetadataList>
              <MetadataListItem label="Pays from">{c.account_name}</MetadataListItem>
              <MetadataListItem label="Payments">{MODE[c.mode] ?? c.mode}</MetadataListItem>
              <MetadataListItem label="Updates">
                {c.webhook_id ? 'As they happen' : 'Checked every minute'}
              </MetadataListItem>
            </MetadataList>
            <HStack gap={2} hAlign="between" vAlign="center" wrap="wrap">
              <Connected c={c} />
              <HStack gap={2}>
                <Button label="Replace" variant="ghost" onClick={() => setReplacing(true)} />
                <Disconnect
                  c={c}
                  consequence="No new payments go through Mercury. Payments already sent are still tracked until they finish. Bills released afterwards are paid outside Payvy."
                  onDone={onChange}
                />
              </HStack>
            </HStack>
          </VStack>
        ) : (
          <VStack gap={4}>
            {!c && (
              <Text type="supporting">
                No bank pays bills yet. Until one does, you release a bill, pay it however you normally do, and mark it
                paid.
              </Text>
            )}
            {c && <Text type="supporting">Connecting again replaces {c.account_name}.</Text>}
            <ConnectMercury
              onCancel={c ? () => setReplacing(false) : undefined}
              onConnected={() => {
                setReplacing(false);
                onChange();
              }}
            />
          </VStack>
        )}
      </Card>
    </VStack>
  );
}

/** Mercury lists the accounts to choose from in its refusal; offer them as a choice. */
function accountChoices(err: string): { value: string; label: string }[] | null {
  const m = /pick the account bills are paid from with account_id: (.+)$/.exec(err);
  if (!m) return null;
  const out = [...(m[1] ?? '').matchAll(/(.+?) \(([^)]+)\)(?:, |$)/g)].map((x) => ({
    value: x[2] ?? '',
    label: x[1] ?? '',
  }));
  return out.length ? out : null;
}

function ConnectMercury({ onConnected, onCancel }: { onConnected: () => void; onCancel?: () => void }) {
  const [mode, setMode] = useState<'request' | 'direct'>('request');
  const [token, setToken] = useState('');
  const [sandbox, setSandbox] = useState(false);
  const [choices, setChoices] = useState<{ value: string; label: string }[] | null>(null);
  const [account, setAccount] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  const toast = useToast();

  async function connect() {
    setError(null);
    try {
      await run('bank.connect', {
        provider: 'mercury',
        token,
        mode,
        environment: sandbox ? 'sandbox' : 'production',
        ...(account ? { account_id: account } : {}),
      });
      toast({ body: 'Mercury connected' });
      onConnected();
    } catch (e) {
      const m = message(e);
      const options = accountChoices(m);
      if (options) setChoices(options);
      else setError(m);
    }
  }

  return (
    <VStack gap={4}>
      <RadioList
        label="How payments are sent"
        value={mode}
        onChange={(v) => setMode(v === 'direct' ? 'direct' : 'request')}
      >
        <RadioListItem
          value="request"
          label="Approve each one in Mercury (recommended)"
          description="Payvy asks Mercury to pay; a Mercury admin approves it there. Use a Custom token with the RequestSendMoney scope."
        />
        <RadioListItem
          value="direct"
          label="Send at once"
          description="Mercury pays as soon as a person releases the bill in Payvy. Use a Read and Write token, with this server's IP allowed in Mercury."
        />
      </RadioList>
      <TextInput
        label="Mercury API token"
        type="password"
        description="In Mercury: Settings → Tokens. To let Payvy invite vendors to enter their bank details, also give the token permission to create recipient invites. The token is shown once there; Payvy never shows it again."
        value={token}
        onChange={setToken}
        autoComplete="off"
      />
      <CheckboxInput label="Sandbox account" description="For testing with Mercury's sandbox." value={sandbox} onChange={setSandbox} />
      {choices && (
        <Selector
          label="Pay bills from"
          placeholder="Choose an account"
          options={choices}
          value={account}
          onChange={setAccount}
        />
      )}
      {choices && !account && <Text type="supporting">This token can use more than one account. Choose the one bills are paid from.</Text>}
      {error && <Banner status="error" title={error} />}
      <HStack gap={2}>
        <Button
          label="Connect Mercury"
          variant="primary"
          isDisabled={!token.trim() || (!!choices && !account)}
          clickAction={connect}
        />
        {onCancel && <Button label="Cancel" variant="ghost" onClick={onCancel} />}
      </HStack>
    </VStack>
  );
}

// ── Reads balances ──────────────────────────────────────────────────────────

function ReadingBank({ connection, onChange }: { connection: Connection | null; onChange: () => void }) {
  const [accounts, setAccounts] = useState<Account[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setAccounts(null);
    setError(null);
    if (!connection) return;
    run<{ accounts: Account[] }>('bank.accounts', {})
      .then((r) => setAccounts(r.result.accounts))
      .catch((e) => setError(message(e)));
  }, [connection]);

  const c = connection;
  return (
    <VStack gap={3}>
      <VStack gap={1}>
        <Heading level={2}>Reads balances</Heading>
        <Text type="supporting">
          A bank Payvy only reads: balances and transactions, for you and your agents. It never pays a bill.
        </Text>
      </VStack>
      <Card>
        {c ? (
          <VStack gap={4}>
            <HStack gap={2} vAlign="center" wrap="wrap">
              <Text weight="semibold">Rho</Text>
              <Badge label="Read only" />
              {c.environment === 'sandbox' && <Badge label="Sandbox" variant="warning" />}
            </HStack>
            {error && <Banner status="error" title="Couldn't read Rho" description={error} />}
            {!accounts && !error && <Spinner label="Reading accounts" />}
            {accounts && (
              <VStack gap={2}>
                {accounts.map((a) => (
                  <HStack key={a.id} gap={3} hAlign="between">
                    <Text>{a.name}</Text>
                    <Text hasTabularNumbers weight={a.available_cents ? 'semibold' : undefined}>
                      {money(a.available_cents)}
                    </Text>
                  </HStack>
                ))}
              </VStack>
            )}
            <HStack gap={2} hAlign="between" vAlign="center" wrap="wrap">
              <Connected c={c} />
              <Disconnect
                c={c}
                consequence="Payvy stops reading Rho. Nothing about paying bills changes."
                onDone={onChange}
              />
            </HStack>
          </VStack>
        ) : (
          <ConnectRho onConnected={onChange} />
        )}
      </Card>
    </VStack>
  );
}

function ConnectRho({ onConnected }: { onConnected: () => void }) {
  const [token, setToken] = useState('');
  const [sandbox, setSandbox] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const toast = useToast();

  async function connect() {
    setError(null);
    try {
      await run('bank.connect', { provider: 'rho', token, environment: sandbox ? 'sandbox' : 'production' });
      toast({ body: 'Rho connected' });
      onConnected();
    } catch (e) {
      setError(message(e));
    }
  }

  return (
    <VStack gap={4}>
      <Text type="supporting">Not connected.</Text>
      <TextInput
        label="Rho API token"
        type="password"
        description="In Rho: Settings → API → Access Tokens, with accounts:read and transactions:read only."
        value={token}
        onChange={setToken}
        autoComplete="off"
      />
      <CheckboxInput label="Sandbox account" description="For testing with Rho's sandbox." value={sandbox} onChange={setSandbox} />
      {error && <Banner status="error" title={error} />}
      <HStack gap={2}>
        <Button label="Connect Rho" variant="primary" isDisabled={!token.trim()} clickAction={connect} />
      </HStack>
    </VStack>
  );
}
