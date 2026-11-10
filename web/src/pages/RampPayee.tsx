import { useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Selector } from '@astryxdesign/core/Selector';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { money } from '../money';

interface Destination { recipient_id: string; instrument_id: string; destination: string; fingerprint: string; fee: { amount: number } }
const message = (e: unknown) => e instanceof Error ? e.message : String(e);
export function RampPayee({ vendorId, onChanged }: { vendorId: string; onChanged: () => Promise<void> }) {
  const [connection, setConnection] = useState<string | null>(null);
  const [vendors, setVendors] = useState<{ id: string; name: string }[]>([]);
  const [recipient, setRecipient] = useState<string | undefined>();
  const [destinations, setDestinations] = useState<Destination[]>([]);
  const [instrument, setInstrument] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState(false);
  useEffect(() => {
    let active = true;
    (async () => {
      const b = await run<{ connection: { id: string; provider: string } | null }>('bank.get', {});
      if (b.result.connection?.provider !== 'ramp' || !active) return;
      setConnection(b.result.connection.id);
      const r = await run<{ connection_id: string; vendors: { id: string; name: string }[] }>('bank.recipients', {});
      if (active) { setConnection(r.result.connection_id); setVendors(r.result.vendors); }
    })().catch(e => { if (active) setError(message(e)); });
    return () => { active = false; };
  }, [vendorId]);
  useEffect(() => {
    let active = true;
    setDestinations([]); setInstrument(undefined); setSent(false);
    if (!recipient) return;
    setBusy(true); setError(null);
    run<{ connection_id: string; destinations: Destination[] }>('bank.recipients', { recipient_id: recipient })
      .then(r => { if (active) { setConnection(r.result.connection_id); setDestinations(r.result.destinations); } })
      .catch(e => { if (active) setError(message(e)); })
      .finally(() => { if (active) setBusy(false); });
    return () => { active = false; };
  }, [recipient]);
  if (!connection) return error ? <Banner status="error" title={error} /> : null;
  return <VStack gap={3}>
    <Text weight="semibold">Map an existing Ramp payee</Text>
    <Text type="supporting">Choose the vendor and the specific ACH account. A person accepts the proposal in Waiting on you. Ramp payments remain disabled.</Text>
    <Selector label="Vendor in Ramp" placeholder="Choose a Ramp vendor" value={recipient} onChange={setRecipient} isDisabled={busy}
      options={vendors.map(v => ({ value: v.id, label: v.name }))} />
    {recipient && !busy && destinations.length === 0 && !error && <Text>No eligible ACH accounts were returned for this vendor.</Text>}
    {destinations.length > 0 && <Selector label="Ramp ACH account" placeholder="Choose an account" value={instrument} onChange={v => { setInstrument(v); setSent(false); }} isDisabled={busy}
      options={destinations.map(d => ({ value: d.instrument_id, label: `${d.destination} · Fee ${money(d.fee.amount)}` }))} />}
    {error && <Banner status="error" title={error} />}
    {sent && <Text>Proposed. Review it in Waiting on you.</Text>}
    <Button label="Propose Ramp payee" variant="secondary" isDisabled={busy || !instrument || sent} onClick={async () => {
      const d = destinations.find(d => d.instrument_id === instrument);
      if (!d) return;
      setBusy(true); setError(null);
      try {
        await run('vendor.propose_payee', { vendor_id: vendorId, connection_id: connection, recipient_id: d.recipient_id,
          method: 'ach', instrument_id: d.instrument_id, instrument_fingerprint: d.fingerprint });
        setSent(true); await onChanged();
      } catch (e) { setError(message(e)); } finally { setBusy(false); }
    }} />
  </VStack>;
}
