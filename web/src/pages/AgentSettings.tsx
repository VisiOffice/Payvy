// Settings > Agents: API keys for AI agents (MCP or the command API). An
// agent prepares work and never signs off; agent_key.* are person-only Admin
// commands, so an agent can't mint or revoke keys (it may list them). The key is in the create
// result only; Payvy keeps a hash, so this page shows it once and forgets it.
import { useCallback, useEffect, useState } from 'react';
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

export function AgentSettings() {
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
    <Page title="Agents" narrow
      description="Give an AI agent its own key to work in Payvy over MCP or the API. Agents read invoices, enter bills and prepare payments; approving and paying stay with people.">
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
    </Page>
  );
}
