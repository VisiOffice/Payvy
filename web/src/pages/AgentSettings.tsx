// Settings > Agents, two tabs. Reading: the company's own Anthropic or OpenAI
// key for Payvy's reader (reader_key.*), so invoices are read as they arrive
// and the provider bills the company. MCP: API keys for outside agents (MCP or
// the command API). An agent prepares work and never signs off; agent_key.* are person-only Admin
// commands, so an agent can't mint or revoke keys (it may list them). The key is in the create
// result only; Payvy keeps a hash, so this page shows it once and forgets it.
import { useCallback, useEffect, useState } from 'react';
import { useSearchParams } from 'react-router';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Tab, TabList } from '@astryxdesign/core/TabList';
import { AlertDialog } from '@astryxdesign/core/AlertDialog';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { HStack } from '@astryxdesign/core/HStack';
import { List, ListItem } from '@astryxdesign/core/List';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { date } from '../money';
import { Page } from '../Page';
import { useSession } from '../session';

interface AgentKey {
  id: string;
  actor: string;
  created_at: string;
  revoked_at: string | null;
}

interface Made extends AgentKey {
  key: string;
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

function McpTab() {
  const toast = useToast();
  const [keys, setKeys] = useState<AgentKey[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [made, setMade] = useState<Made | null>(null);
  const [revoking, setRevoking] = useState<AgentKey | null>(null);
  const mcp = `${window.location.origin}/mcp`;

  const load = useCallback(async () => {
    try {
      setKeys((await run<{ keys: AgentKey[] }>('agent_key.list', {})).result.keys);
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);
  useEffect(() => { void load(); }, [load]);

  async function create() {
    setBusy(true);
    try {
      const r = (await run<Made>('agent_key.create', { name: name.trim() })).result;
      setMade(r);
      setName('');
      await load();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally { setBusy(false); }
  }

  async function revoke() {
    if (!revoking) return;
    setBusy(true);
    try {
      await run('agent_key.revoke', { id: revoking.id });
      toast({ body: `Key for ${revoking.actor} revoked` });
      if (made?.id === revoking.id) setMade(null);
      setRevoking(null);
      await load();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally { setBusy(false); }
  }

  async function copy(text: string, what: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast({ body: `${what} copied` });
    } catch {
      toast({ body: `Couldn't copy; select the ${what.toLowerCase()} and copy it`, type: 'error' });
    }
  }

  return (
    <VStack gap={4}>
      <Text type="supporting">For an AI agent that works in Payvy itself, such as Claude with Payvy added as an MCP server. It reads invoices, enters bills and prepares payments under its own name; approving and paying stay with people.</Text>
      {error && <Banner status="error" title="Couldn't load agent keys" description={error} />}
      {made && (
        <Card>
          <VStack gap={3}>
            <Text weight="semibold">Key for {made.actor}: copy it now</Text>
            <Text type="supporting">This is the only time Payvy shows it. Paste it into the agent's settings or your password manager. If it's lost, revoke it and make another.</Text>
            <HStack gap={2} vAlign="end" wrap="wrap">
              <div style={{ flex: '1 1 20rem', minWidth: 0 }}>
                <TextInput label="API key" value={made.key} onChange={() => {}} isReadOnly />
              </div>
              <Button label="Copy key" variant="primary" onClick={() => copy(made.key, 'Key')} />
            </HStack>
            <HStack gap={2} vAlign="end" wrap="wrap">
              <div style={{ flex: '1 1 20rem', minWidth: 0 }}>
                <TextInput label="MCP server URL" value={mcp} onChange={() => {}} isReadOnly />
              </div>
              <Button label="Copy URL" variant="secondary" onClick={() => copy(mcp, 'URL')} />
            </HStack>
            <Text type="supporting">Send the key as a header: Authorization: Bearer &lt;key&gt;.</Text>
            <Button label="I've saved it" variant="ghost" onClick={() => setMade(null)} />
          </VStack>
        </Card>
      )}
      <Card>
        <VStack gap={3}>
          <Text weight="semibold">New agent key</Text>
          <TextInput label="Agent name" value={name} onChange={setName} placeholder="quarry-agent"
            description="What the agent's work is recorded under: lowercase letters, digits, . _ -" />
          <HStack>
            <Button label="Create key" variant="primary" clickAction={create} isDisabled={busy || !name.trim()} />
          </HStack>
        </VStack>
      </Card>
      {!keys && !error && <Spinner label="Loading agent keys" />}
      {keys && keys.length > 0 && (
        <Card>
          <List hasDividers edge="bleed">
            {keys.map(k => (
              <ListItem key={k.id} itemKey={k.id}
                label={<HStack gap={2} vAlign="center"><Text weight="semibold">{k.actor}</Text>{k.revoked_at && <Badge label="Revoked" variant="neutral" />}</HStack>}
                description={`Made ${date(k.created_at)}${k.revoked_at ? ` · revoked ${date(k.revoked_at)}` : ''}`}
                endContent={!k.revoked_at && <Button label="Revoke" aria-label={`Revoke key for ${k.actor}`} size="sm" variant="ghost" onClick={() => setRevoking(k)} />}
              />
            ))}
          </List>
        </Card>
      )}
      {keys && keys.length === 0 && <Text type="supporting">No agent keys yet.</Text>}
      <AlertDialog
        isOpen={revoking !== null}
        onOpenChange={(open) => !open && !busy && setRevoking(null)}
        title={`Revoke the key for ${revoking?.actor ?? 'this agent'}?`}
        description="The agent's next call with this key is refused. What it already did stays on the record. This can't be undone; make a new key to let it back in."
        actionLabel="Revoke"
        isActionLoading={busy}
        onAction={revoke}
      />
    </VStack>
  );
}

interface ReaderKey {
  configured: boolean;
  provider?: 'anthropic' | 'openai';
  model?: string;
  key_last4?: string;
  set_at?: string;
  set_by?: string;
  default_models?: { anthropic: string };
}

const PROVIDERS = { anthropic: 'Anthropic (Claude)', openai: 'OpenAI' } as const;

function ReadingTab() {
  const toast = useToast();
  const { me, setMe } = useSession();
  const [key, setKey] = useState<ReaderKey | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [provider, setProvider] = useState<'anthropic' | 'openai'>('anthropic');
  const [apiKey, setApiKey] = useState('');
  const [model, setModel] = useState('');
  const [busy, setBusy] = useState(false);
  const [removing, setRemoving] = useState(false);

  const load = useCallback(async () => {
    try {
      setKey((await run<ReaderKey>('reader_key.get', {})).result);
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);
  useEffect(() => { void load(); }, [load]);

  /** The session's plan says whether Read buttons show; refresh it. */
  async function refreshPlan() {
    try {
      const org = (await run<{ plan: NonNullable<typeof me.plan> }>('org.get', {})).result;
      setMe({ ...me, plan: { ...me.plan, ...org.plan, own_reader_key: !!org.plan.own_reader_key } });
    } catch { /* the next sign-in has it */ }
  }

  async function save() {
    setBusy(true);
    try {
      await run('reader_key.set', { provider, api_key: apiKey.trim(), ...(model.trim() ? { model: model.trim() } : {}) });
      setApiKey('');
      setModel('');
      setEditing(false);
      toast({ body: 'Key checked and saved. New invoices are read with it.' });
      await load();
      await refreshPlan();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally { setBusy(false); }
  }

  async function remove() {
    setBusy(true);
    try {
      await run('reader_key.remove', {});
      setRemoving(false);
      toast({ body: 'Key removed' });
      await load();
      await refreshPlan();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally { setBusy(false); }
  }

  const managed = !!me.plan?.hosted && me.plan.reader && !me.plan.own_reader_key;
  const form = (
    <VStack gap={3}>
      <SegmentedControl label="Provider" value={provider} onChange={(v) => setProvider(v as 'anthropic' | 'openai')}>
        <SegmentedControlItem value="anthropic" label={PROVIDERS.anthropic} />
        <SegmentedControlItem value="openai" label={PROVIDERS.openai} />
      </SegmentedControl>
      <TextInput label="API key" type="password" value={apiKey} onChange={setApiKey}
        placeholder={provider === 'anthropic' ? 'sk-ant-…' : 'sk-…'}
        description="Checked with the provider before it's saved (free), then sealed on the server. Payvy never shows it again." />
      <TextInput label={provider === 'anthropic' ? 'Model (optional)' : 'Model'} value={model} onChange={setModel}
        placeholder={provider === 'anthropic' ? key?.default_models?.anthropic ?? '' : 'A model that reads PDFs and images'}
        description={provider === 'anthropic' ? "Leave empty for the model Payvy uses." : undefined} />
      <HStack gap={2}>
        <Button label="Check and save key" variant="primary" clickAction={save}
          isDisabled={busy || !apiKey.trim() || (provider === 'openai' && !model.trim())} />
        {key?.configured && <Button label="Cancel" variant="ghost" onClick={() => setEditing(false)} />}
      </HStack>
    </VStack>
  );

  return (
    <VStack gap={4}>
      <Text type="supporting">
        Payvy reads each invoice as it arrives and drafts the bill. With your own Anthropic or OpenAI key, it
        reads with that key and the provider bills the reading to you.
        {managed ? ' Your plan already includes Payvy reading for you; a key of your own moves that cost to your provider account.' : ''}
      </Text>
      {error && <Banner status="error" title="Couldn't load the reading settings" description={error} />}
      {!key && !error && <Spinner label="Loading" />}
      {key && key.configured && !editing && (
        <Card>
          <VStack gap={3}>
            <HStack gap={2} vAlign="center">
              <Text weight="semibold">Reading with your {PROVIDERS[key.provider!]} key</Text>
              <Badge label="On" variant="success" />
            </HStack>
            <Text type="supporting">
              Key ending {key.key_last4} · {key.model} · set by {key.set_by} on {date(key.set_at!)}.
              Invoices that arrive from then on are read automatically; read earlier ones from Documents.
            </Text>
            <HStack gap={2}>
              <Button label="Replace key" variant="secondary" onClick={() => { setProvider(key.provider!); setEditing(true); }} />
              <Button label="Remove" variant="ghost" onClick={() => setRemoving(true)} />
            </HStack>
          </VStack>
        </Card>
      )}
      {key && (!key.configured || editing) && (
        <Card>
          <VStack gap={3}>
            <Text weight="semibold">{key.configured ? 'Replace the key' : 'Use your own model key'}</Text>
            {form}
          </VStack>
        </Card>
      )}
      <AlertDialog
        isOpen={removing}
        onOpenChange={(open) => !open && !busy && setRemoving(false)}
        title="Stop reading with your key?"
        description="Payvy forgets the key. Reading then follows your plan: without Payvy reading on it, invoices wait for your agent or for you. Delete the key at the provider too if nothing else uses it."
        actionLabel="Remove"
        isActionLoading={busy}
        onAction={remove}
      />
    </VStack>
  );
}

export function AgentSettings() {
  const [params, setParams] = useSearchParams();
  const tab = params.get('tab') === 'mcp' ? 'mcp' : 'reading';
  return (
    <Page title="Agents" narrow description="How invoices get read, and the AI agents that work in Payvy.">
      <VStack gap={4}>
        <TabList value={tab} onChange={(v) => setParams(v === 'reading' ? {} : { tab: v }, { replace: true })} hasDivider role="tablist">
          <Tab value="reading" label="Reading invoices" panelId="agents-reading" />
          <Tab value="mcp" label="MCP agents" panelId="agents-mcp" />
        </TabList>
        <div role="tabpanel" id={tab === 'reading' ? 'agents-reading' : 'agents-mcp'}>
          {tab === 'reading' ? <ReadingTab /> : <McpTab />}
        </div>
      </VStack>
    </Page>
  );
}
