// Settings > Ledger: connect VisiBooks, read its accounts and vendors
// (ledger.sync), say how bills post, and link each vendor to its ledger
// vendor. The API key is write-only: sealed by the server at connect and
// never shown again. Payvy never moves money and never owns the books; it
// posts what the bank already did (or is about to do, at approval) to the
// company's own ledger, per company, by an Admin.
import { useCallback, useEffect, useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { HStack } from '@astryxdesign/core/HStack';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Selector } from '@astryxdesign/core/Selector';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { useToast } from '@astryxdesign/core/Toast';
import { Page } from '../Page';
import { preview, run } from '../api/client';
import type { CommandName } from '../api/commands.gen';

interface Account {
  id: string;
  name: string;
  account_type?: string | null;
  sub_type?: string | null;
  inactive: boolean;
}

interface LedgerSettings {
  connected: boolean;
  provider?: string;
  origin?: string | null;
  entity?: string;
  post_when?: 'approval' | 'payment';
  ap_account_id?: string | null;
  defaults?: Record<string, unknown>;
  bank_account_map?: Record<string, string>;
  status?: string;
  catalog_synced_at?: string | null;
  accounts?: Account[];
}

interface BankConnection {
  id: string;
  provider: string;
  account_name: string;
  capability: string;
}

interface VendorRow {
  vendor_id: string;
  name: string;
  linked: { external_id: string; name: string; archived: boolean } | null;
  suggestion: { external_id: string; name: string; reason: 'external_id' | 'name' } | null;
}

interface Matches {
  connected: boolean;
  vendors: VendorRow[];
  ledger_vendors: { external_id: string; name: string; linked_to: string | null }[];
  summary?: { linked: number; suggested: number; unmatched: number };
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Preview, then commit with the token: the confirmed tier's two steps. */
async function confirmed<R>(name: CommandName, input: never) {
  const p = await preview<R>(name, input);
  return run<R>(name, input, p.confirm_token);
}

export function LedgerSettings() {
  const [data, setData] = useState<LedgerSettings | null>(null);
  const [matches, setMatches] = useState<Matches | null>(null);
  const [banks, setBanks] = useState<BankConnection[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [origin, setOrigin] = useState('');
  const [entity, setEntity] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [postWhen, setPostWhen] = useState<'approval' | 'payment'>('approval');
  const [apAccount, setApAccount] = useState('');
  const [expenseAccount, setExpenseAccount] = useState('');
  const [bankMap, setBankMap] = useState<Record<string, string>>({});
  const [picks, setPicks] = useState<Record<string, string>>({});
  const toast = useToast();

  const load = useCallback(async () => {
    try {
      const s = (await run<LedgerSettings>('ledger.settings', {})).result;
      setData(s);
      if (s.post_when) setPostWhen(s.post_when);
      setApAccount(s.ap_account_id ?? '');
      const expense = s.defaults?.expense_account_id;
      setExpenseAccount(typeof expense === 'string' ? expense : '');
      setBankMap(s.bank_account_map ?? {});
      if (s.connected) {
        setMatches((await run<Matches>('ledger.vendor_matches', {})).result);
        const b = (await run<{ connections: BankConnection[] }>('bank.get', {})).result;
        setBanks((b.connections ?? []).filter(c => c.capability === 'pay'));
      }
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);
  useEffect(() => { void load(); }, [load]);

  async function connect() {
    setBusy(true);
    try {
      const input = { origin: origin.trim(), entity: entity.trim(), api_key: apiKey, post_when: postWhen };
      await confirmed<LedgerSettings>('ledger.connect', input as never);
      setApiKey('');
      toast({ body: 'VisiBooks connected. Syncing its accounts and vendors…' });
      await sync();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally { setBusy(false); }
  }

  async function sync() {
    setBusy(true);
    try {
      const r = (await run<{ counts: Record<string, { total: number }>; vendors: { linked: number; suggested: number; unmatched: number } }>('ledger.sync', {})).result;
      toast({ body: `Synced ${r.counts.accounts?.total ?? 0} accounts and ${r.counts.vendors?.total ?? 0} vendors from VisiBooks` });
      await load();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally { setBusy(false); }
  }

  async function save() {
    setBusy(true);
    try {
      const defaults = { ...(data?.defaults ?? {}) } as Record<string, unknown>;
      if (expenseAccount) defaults.expense_account_id = expenseAccount; else delete defaults.expense_account_id;
      const map = Object.fromEntries(Object.entries(bankMap).filter(([, v]) => v));
      await confirmed<LedgerSettings>('ledger.configure', {
        post_when: postWhen,
        ap_account_id: apAccount || null,
        defaults,
        bank_account_map: map,
      } as never);
      toast({ body: 'Ledger settings saved' });
      await load();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
      await load();
    } finally { setBusy(false); }
  }

  async function link(vendor: VendorRow, externalId: string | null) {
    setBusy(true);
    try {
      await run('ledger.link_vendor', { vendor_id: vendor.vendor_id, external_id: externalId });
      toast({ body: externalId ? `${vendor.name} linked` : `${vendor.name} unlinked` });
      await load();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally { setBusy(false); }
  }

  const accounts = (data?.accounts ?? []).filter(a => !a.inactive);
  const option = (a: Account) => ({ value: a.id, label: a.name });
  const expenseOptions = accounts.filter(a => !a.account_type || a.account_type === 'expense').map(option);
  const apOptions = accounts.filter(a => !a.account_type || a.account_type === 'liability').map(option);
  const bankOptions = accounts
    .filter(a => !a.account_type || (a.account_type === 'asset' && (a.sub_type === 'bank' || a.sub_type === 'cash')))
    .map(option);
  const free = (matches?.ledger_vendors ?? []).filter(v => !v.linked_to);
  const synced = data?.catalog_synced_at ? new Date(data.catalog_synced_at).toLocaleString() : null;

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
              <HStack hAlign="between" vAlign="center">
                <VStack gap={1}>
                  <Text weight="semibold">VisiBooks · entity {data.entity}</Text>
                  <Text type="supporting">{data.origin} · {data.status}</Text>
                </VStack>
                <Button label={synced ? 'Sync now' : 'Sync accounts and vendors'} variant={synced ? 'secondary' : 'primary'} clickAction={sync} isDisabled={busy} />
              </HStack>
              <Text type="supporting">
                {synced
                  ? `Accounts, periods and vendors last read from VisiBooks ${synced}. Bills are checked against them before they post.`
                  : 'Nothing has been read from VisiBooks yet. Sync before posting: bills are checked against its accounts, periods and vendors.'}
              </Text>
            </VStack>
          </Card>

          <Card>
            <VStack gap={3}>
              <Text weight="semibold">How bills post</Text>
              <SegmentedControl label="Post bills" value={postWhen} onChange={(v) => setPostWhen(v as 'approval' | 'payment')}>
                <SegmentedControlItem value="approval" label="When approved" />
                <SegmentedControlItem value="payment" label="When paid" />
              </SegmentedControl>
              <Text type="supporting">
                When approved: a bill posts as soon as its approval completes, and its payment posts when the bank
                sends it. When paid: bill and payment post together at payment. A bill paid outside Payvy posts its
                bill and leaves the payment leg out (it has no Payvy bank account to draw on).
              </Text>
              {accounts.length > 0 ? (
                <>
                  <Selector label="Accounts payable account" placeholder="Choose an account" options={apOptions} value={apAccount} onChange={setApAccount} />
                  <Selector label="Default expense account" placeholder="Choose an account" options={expenseOptions} value={expenseAccount} onChange={setExpenseAccount}
                    description="For bill lines with no account, when the vendor has no default expense account in VisiBooks." />
                  {banks.map(b => (
                    <Selector key={b.id} label={`${b.account_name} (${b.provider}) pays from`} placeholder="Choose a bank account in VisiBooks"
                      options={bankOptions} value={bankMap[b.id] ?? ''} onChange={v => setBankMap(m => ({ ...m, [b.id]: v }))}
                      description="Payments sent through this bank post against this account. Without one, a payment waits until it is mapped." />
                  ))}
                  {banks.length === 0 && <Text type="supporting">Connect the bank that pays (Settings → Bank) to map it to a VisiBooks bank account.</Text>}
                </>
              ) : (
                <Text type="supporting">Sync to choose the accounts payable, default expense and bank accounts by name.</Text>
              )}
              <Button label="Save ledger settings" variant="primary" clickAction={save} isDisabled={busy} />
            </VStack>
          </Card>

          <Card>
            <VStack gap={3}>
              <Text weight="semibold">Vendors in VisiBooks</Text>
              <Text type="supporting">
                Each vendor's bills post to the VisiBooks vendor linked here. Payvy suggests a match (the same vendor id from another system,
                else the same name); a person links it. Once a bill has posted, the link can't change.
              </Text>
              {matches?.summary && (
                <HStack gap={2}>
                  <Badge label={`${matches.summary.linked} linked`} variant="success" />
                  <Badge label={`${matches.summary.suggested} suggested`} variant="info" />
                  <Badge label={`${matches.summary.unmatched} not matched`} variant="neutral" />
                </HStack>
              )}
              {!synced && <Text type="supporting">Sync first to see VisiBooks' vendors.</Text>}
              {synced && (matches?.vendors ?? []).map(v => (
                <HStack key={v.vendor_id} gap={3} hAlign="between" vAlign="center" wrap="wrap">
                  <VStack gap={1}>
                    <Text weight="semibold">{v.name}</Text>
                    <Text type="supporting">
                      {v.linked
                        ? `Linked to ${v.linked.name || v.linked.external_id}${v.linked.archived ? ' (archived in VisiBooks)' : ''}`
                        : v.suggestion
                          ? `Suggested: ${v.suggestion.name} (${v.suggestion.reason === 'external_id' ? 'same vendor id' : 'same name'})`
                          : 'No match in VisiBooks'}
                    </Text>
                  </VStack>
                  <HStack gap={2} vAlign="end">
                    {v.linked && <Button label="Unlink" variant="ghost" size="sm" clickAction={() => link(v, null)} isDisabled={busy} />}
                    {!v.linked && v.suggestion && (
                      <Button label="Link" variant="primary" size="sm" clickAction={() => link(v, v.suggestion!.external_id)} isDisabled={busy} />
                    )}
                    {!v.linked && free.length > 0 && (
                      <>
                        <Selector label="Or choose" placeholder="Choose a VisiBooks vendor" size="sm"
                          options={free.map(f => ({ value: f.external_id, label: f.name }))}
                          value={picks[v.vendor_id] ?? ''} onChange={x => setPicks(p => ({ ...p, [v.vendor_id]: x }))} />
                        <Button label="Link chosen" variant="secondary" size="sm" clickAction={() => link(v, picks[v.vendor_id] ?? null)}
                          isDisabled={busy || !picks[v.vendor_id]} />
                      </>
                    )}
                  </HStack>
                </HStack>
              ))}
            </VStack>
          </Card>
        </VStack>
      )}
    </Page>
  );
}
