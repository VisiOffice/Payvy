// Settings > Purchase orders: which outside systems may send purchase
// orders in (po.source_authorize / po.source_list / po.source_revoke). A
// grant names one agent key, one source name, the vendors it covers and a
// per-PO limit: POs it sends within the limit are active at once, above it
// they wait as drafts for a person. Only an admin grants or revokes.
import { useCallback, useEffect, useState } from 'react';
import { AlertDialog } from '@astryxdesign/core/AlertDialog';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { HStack } from '@astryxdesign/core/HStack';
import { List, ListItem } from '@astryxdesign/core/List';
import { MultiSelector } from '@astryxdesign/core/MultiSelector';
import { Selector } from '@astryxdesign/core/Selector';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { useToast } from '@astryxdesign/core/Toast';
import { Link } from 'react-router';
import { run } from '../api/client';
import { cents, money } from '../money';
import { useSession } from '../session';

interface Grant {
  id: string;
  source: string;
  key_id: string;
  key_actor: string | null;
  key_revoked: boolean;
  vendors: { id: string; name: string }[];
  max_cents: number;
  owner: string;
  authorized_by: string;
  created_at: string;
  revoked_at: string | null;
  revoked_by: string | null;
}
interface AgentKey { id: string; actor: string; revoked_at: string | null }
interface Person { actor: string; name: string }
interface Vendor { id: string; name: string }

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
const date = (s: string) => new Date(s).toLocaleDateString();

export function PoSourceGrants() {
  const { me } = useSession();
  const toast = useToast();
  const [grants, setGrants] = useState<Grant[] | null>(null);
  const [keys, setKeys] = useState<AgentKey[]>([]);
  const [people, setPeople] = useState<Person[]>([]);
  const [vendors, setVendors] = useState<Vendor[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [source, setSource] = useState('');
  const [keyId, setKeyId] = useState('');
  const [vendorIds, setVendorIds] = useState<string[]>([]);
  const [limit, setLimit] = useState('');
  const [owner, setOwner] = useState(me.person.actor);
  const [busy, setBusy] = useState(false);
  const [revoking, setRevoking] = useState<Grant | null>(null);

  const load = useCallback(async () => {
    try {
      const [g, k, p, v] = await Promise.all([
        run<{ authorizations: Grant[] }>('po.source_list', {}),
        run<{ keys: AgentKey[] }>('agent_key.list', {}),
        run<{ people: Person[] }>('person.list', {}),
        run<{ vendors: Vendor[] }>('vendor.list', {}),
      ]);
      setGrants(g.result.authorizations);
      setKeys(k.result.keys.filter(x => !x.revoked_at));
      setPeople(p.result.people);
      setVendors(v.result.vendors);
      setError(null);
    } catch (e) { setError(message(e)); }
  }, []);
  useEffect(() => { void load(); }, [load]);

  const limitCents = cents(limit);
  const ready = source.trim() && keyId && vendorIds.length > 0 && limitCents && limitCents > 0 && owner;

  async function grant() {
    if (!ready) return;
    setBusy(true);
    try {
      await run('po.source_authorize', { source: source.trim(), key_id: keyId, vendor_ids: vendorIds as [string, ...string[]], max_cents: limitCents, owner });
      toast({ body: `${keys.find(k => k.id === keyId)?.actor ?? 'The agent'} can now send purchase orders for ${source.trim()}` });
      setSource(''); setKeyId(''); setVendorIds([]); setLimit('');
      await load();
    } catch (e) { toast({ body: message(e), type: 'error' }); }
    finally { setBusy(false); }
  }

  async function revoke() {
    if (!revoking) return;
    setBusy(true);
    try {
      await run('po.source_revoke', { authorization_id: revoking.id });
      toast({ body: `Stopped ${revoking.source} from sending purchase orders` });
      setRevoking(null);
      await load();
    } catch (e) { toast({ body: message(e), type: 'error' }); }
    finally { setBusy(false); }
  }

  return <VStack gap={3}>
    <VStack gap={1}>
      <Text weight="semibold">Systems that send purchase orders</Text>
      <Text type="supporting">Let another system, such as your order software, send POs through an agent key. POs within the limit are active at once; above it they wait as drafts for someone to activate. Only that key can change the POs it sends, and not after a person edits one.</Text>
    </VStack>
    {error && <Banner status="error" title="Couldn't load PO senders" description={error} />}
    {!grants && !error && <Spinner label="Loading PO senders" />}
    {grants && grants.length > 0 && <Card>
      <List hasDividers edge="bleed">
        {grants.map(g => {
          const off = g.revoked_at || g.key_revoked;
          return <ListItem key={g.id} itemKey={g.id}
            label={<HStack gap={2} vAlign="center" wrap="wrap">
              <Text weight="semibold">{g.source}</Text>
              <Text type="supporting">via {g.key_actor ?? 'a deleted key'}</Text>
              {g.revoked_at ? <Badge label="Revoked" variant="neutral" /> : g.key_revoked ? <Badge label="Key revoked" variant="neutral" /> : null}
            </HStack>}
            description={<VStack gap={0}>
              <Text type="supporting">Up to {money(g.max_cents)} a PO · {g.vendors.map(v => v.name).join(', ')}</Text>
              <Text type="supporting">Owner {g.owner} · {g.revoked_at ? `revoked ${date(g.revoked_at)}${g.revoked_by ? ` by ${g.revoked_by}` : ''}` : `granted ${date(g.created_at)} by ${g.authorized_by}`}</Text>
            </VStack>}
            endContent={!off && <Button label="Revoke" aria-label={`Revoke ${g.source}`} size="sm" variant="ghost" onClick={() => setRevoking(g)} />}
          />;
        })}
      </List>
    </Card>}
    {grants && grants.length === 0 && <Text type="supporting">No system sends purchase orders yet.</Text>}
    {grants && <Card>
      <VStack gap={3}>
        <Text weight="semibold">Let a system send purchase orders</Text>
        {keys.length === 0
          ? <Text type="supporting">First make an agent key for the system in <Link to="/settings/agents">Settings › Agents</Link>.</Text>
          : <>
            <TextInput label="Source name" value={source} onChange={setSource} placeholder="quarry"
              description="The name the system sends as external_source on each PO. Each of its orders is one PO." />
            <Selector label="Agent key" placeholder="Choose a key" value={keyId} onChange={setKeyId}
              options={keys.map(k => ({ value: k.id, label: k.actor }))}
              description="Only this key may send or change these POs." />
            <MultiSelector label="Vendors" placeholder="Choose vendors" value={vendorIds} onChange={setVendorIds}
              options={vendors.map(v => ({ value: v.id, label: v.name }))} hasSearch={vendors.length > 15}
              description="It can send POs only for these vendors." />
            <TextInput label="Limit per PO" value={limit} onChange={setLimit} placeholder="1,000.00"
              description="A PO above this waits as a draft for a person to activate." />
            <Selector label="Owner" value={owner} onChange={setOwner}
              options={people.map(p => ({ value: p.actor, label: p.name }))}
              description="Recorded as the owner of these POs when the system names none." />
            <HStack>
              <Button label="Grant" variant="primary" clickAction={grant} isDisabled={busy || !ready} />
            </HStack>
          </>}
      </VStack>
    </Card>}
    <AlertDialog
      isOpen={revoking !== null}
      onOpenChange={(open) => !open && !busy && setRevoking(null)}
      title={`Stop ${revoking?.source ?? 'this system'} sending purchase orders?`}
      description="Its next PO is refused. POs it already sent stay as they are, and you can still change them by hand. Grant it again to let it back in."
      actionLabel="Revoke"
      isActionLoading={busy}
      onAction={revoke}
    />
  </VStack>;
}
