import { Allowed } from '../permissions';
import { useEffect, useRef, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Link } from '@astryxdesign/core/Link';
import { Selector } from '@astryxdesign/core/Selector';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { money } from '../money';
import type { RampHealthState } from './RampHealth';

interface Destination { recipient_id: string; instrument_id: string; destination: string; fingerprint: string; fee: { amount: number } }
const message = (e: unknown) => e instanceof Error ? e.message : String(e);
export function RampPayee({ vendorId, vendorName, onChanged }: { vendorId: string; vendorName: string; onChanged: () => Promise<void> }) {
  const [connection, setConnection] = useState<string | null>(null);
  const connectionRef = useRef<string | null>(null);
  const [health, setHealth] = useState<RampHealthState | undefined>();
  const [vendors, setVendors] = useState<{ id: string; name: string }[]>([]);
  const [search, setSearch] = useState(vendorName);
  const [searched, setSearched] = useState(false);
  const [recipient, setRecipient] = useState<string | undefined>();
  const [destinations, setDestinations] = useState<Destination[]>([]);
  const [instrument, setInstrument] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState(false);
  const generation = useRef(0);
  const blocked = health?.status === 'needs_reconnection' || health?.status === 'paused' || health?.status === 'retrying' || health?.access_expired;
  useEffect(() => {
    let active = true;
    const load = async () => {
      const b = await run<{ connection: { id: string; provider: string; health?: RampHealthState } | null }>('bank.get', {});
      if (!active) return;
      const c = b.result.connection?.provider === 'ramp' ? b.result.connection : null;
      if (connectionRef.current !== (c?.id ?? null)) {
        generation.current++;
        setVendors([]); setRecipient(undefined); setDestinations([]); setInstrument(undefined); setSearched(false); setSent(false); setBusy(false);
      }
      connectionRef.current = c?.id ?? null;
      setConnection(c?.id ?? null); setHealth(c?.health);
    };
    const refresh = () => { load().catch(e => { if (active) setError(message(e)); }); };
    refresh(); window.addEventListener('focus', refresh);
    return () => { active = false; generation.current++; window.removeEventListener('focus', refresh); };
  }, [vendorId]);
  useEffect(() => {
    let active = true;
    setDestinations([]); setInstrument(undefined); setSent(false);
    if (!recipient) return;
    setBusy(true); setError(null);
    run<{ connection_id: string; destinations: Destination[] }>('bank.recipients', { recipient_id: recipient })
      .then(r => {
        if (!active) return;
        if (r.result.connection_id !== connectionRef.current) throw new Error('The bank connection changed. Reload this vendor and choose the account again.');
        setDestinations(r.result.destinations);
      })
      .catch(e => { if (active) setError(message(e)); })
      .finally(() => { if (active) setBusy(false); });
    return () => { active = false; };
  }, [recipient]);
  if (!connection) return error ? <Banner status="error" title={error} /> : null;
  return <VStack gap={3}>
    <Text weight="semibold">Map an existing Ramp payee</Text>
    <Text type="supporting">Find the vendor in Ramp, then choose the specific ACH account. A person accepts the proposal in Waiting on you. Ramp payments remain disabled.</Text>
    {blocked && <Banner status="warning" title={health?.message ?? 'Ramp access needs attention'} />}
    {(blocked || error) && <Link href="/settings/bank">Review Ramp connection</Link>}
    <TextInput label="Search Ramp vendors" value={search} onChange={value => {
      setSearch(value); setVendors([]); setRecipient(undefined); setDestinations([]); setInstrument(undefined); setSearched(false); setSent(false); setError(null);
    }} isDisabled={busy || !!blocked} />
    <Button label="Search Ramp" variant="secondary" isDisabled={busy || !!blocked || search.trim().length < 2 || search.trim().length > 120} onClick={async () => {
      const request = ++generation.current;
      setBusy(true); setError(null); setRecipient(undefined); setDestinations([]); setInstrument(undefined); setSent(false); setVendors([]); setSearched(false);
      try {
        const r = await run<{ connection_id: string; vendors: { id: string; name: string }[] }>('bank.recipients', { search: search.trim() });
        if (request !== generation.current) return;
        if (r.result.connection_id !== connectionRef.current) throw new Error('The bank connection changed. Reload this vendor and search again.');
        setVendors(r.result.vendors); setSearched(true);
      } catch (e) { if (request === generation.current) setError(message(e)); }
      finally { if (request === generation.current) setBusy(false); }
    }} />
    {searched && vendors.length === 0 && <Text>No matching active vendors in Ramp. Try another name.</Text>}
    {vendors.length > 0 && <Selector label="Vendor in Ramp" placeholder="Choose a Ramp vendor" value={recipient} onChange={setRecipient} isDisabled={busy || !!blocked}
      options={vendors.map(v => ({ value: v.id, label: vendors.filter(other => other.name === v.name).length > 1 ? `${v.name} · ${v.id.slice(-6)}` : v.name }))} />}
    {recipient && !busy && destinations.length === 0 && !error && <Text>No eligible ACH accounts were returned for this vendor.</Text>}
    {destinations.length > 0 && <Selector label="Ramp ACH account" placeholder="Choose an account" value={instrument} onChange={v => { setInstrument(v); setSent(false); }} isDisabled={busy || !!blocked}
      options={destinations.map(d => ({ value: d.instrument_id, label: `${d.destination} · Fee ${money(d.fee.amount)}${destinations.filter(other => other.destination === d.destination).length > 1 ? ` · ${d.instrument_id.slice(-6)}` : ''}` }))} />}
    {error && <Banner status="error" title={error} />}
    {sent && <Text>Proposed. Review it in Waiting on you.</Text>}
    <Allowed command="vendor.propose_payee"><Button label="Propose Ramp payee" variant="secondary" isDisabled={busy || !!blocked || !instrument || sent} onClick={async () => {
      const d = destinations.find(d => d.instrument_id === instrument);
      if (!d) return;
      setBusy(true); setError(null);
      try {
        await run('vendor.propose_payee', { vendor_id: vendorId, connection_id: connection, recipient_id: d.recipient_id,
          method: 'ach', instrument_id: d.instrument_id, instrument_fingerprint: d.fingerprint });
        setSent(true); await onChanged();
      } catch (e) { setError(message(e)); } finally { setBusy(false); }
    }} /></Allowed>
  </VStack>;
}
