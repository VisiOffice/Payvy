import { useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Dialog, DialogHeader } from '@astryxdesign/core/Dialog';
import { HStack } from '@astryxdesign/core/HStack';
import { Selector } from '@astryxdesign/core/Selector';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { preview, run } from '../api/client';
import type { CommandInputs } from '../api/commands.gen';
import { METHOD } from '../inbox/types';

type Input = CommandInputs['vendor.create_recipient'];
const ACCOUNT_TYPE: Record<string, string> = {
  business_checking: 'Business checking', business_savings: 'Business savings',
  personal_checking: 'Personal checking', personal_savings: 'Personal savings',
};
const message = (e: unknown) => e instanceof Error ? e.message : String(e);

/** A person types the vendor's bank details; Mercury creates the recipient,
 *  and it lands as a proposed payee for someone to accept. The numbers are
 *  sent to Mercury and dropped from this form once it closes. */
export function RecipientDialog({ vendor, onClose, onChanged }: {
  vendor: { id: string; name: string; email: string | null };
  onClose: () => void;
  onChanged: () => Promise<void>;
}) {
  const [method, setMethod] = useState<Input['method']>('ach');
  const [email, setEmail] = useState(vendor.email ?? '');
  const [account, setAccount] = useState('');
  const [routing, setRouting] = useState('');
  const [accountType, setAccountType] = useState<NonNullable<Input['account_type']>>('business_checking');
  const [address1, setAddress1] = useState('');
  const [address2, setAddress2] = useState('');
  const [city, setCity] = useState('');
  const [region, setRegion] = useState('');
  const [postal, setPostal] = useState('');
  const [plan, setPlan] = useState<{ token: string; input: Input; destination: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const check = method === 'check';

  function input(): Input {
    return {
      vendor_id: vendor.id, method, email: email.trim(),
      ...(check ? {} : { account_number: account.replace(/\s/g, ''), routing_number: routing.replace(/\s/g, '') }),
      ...(method === 'ach' ? { account_type: accountType } : {}),
      address: { address1: address1.trim(), ...(address2.trim() ? { address2: address2.trim() } : {}),
        city: city.trim(), region: region.trim(), postal_code: postal.trim() },
    };
  }
  async function review() {
    setBusy(true); setError(null);
    try {
      const i = input();
      const p = await preview<{ destination: string }>('vendor.create_recipient', i);
      if (!p.confirm_token) throw new Error('Could not prepare the recipient. Try again.');
      setPlan({ token: p.confirm_token, input: i, destination: p.result.destination });
    } catch (e) { setError(message(e)); } finally { setBusy(false); }
  }
  async function create() {
    if (!plan) return;
    setBusy(true); setError(null);
    try {
      await run('vendor.create_recipient', plan.input, plan.token);
      await onChanged(); onClose();
    } catch (e) { setError(message(e)); setPlan(null); } finally { setBusy(false); }
  }
  const close = () => { if (!busy) onClose(); };
  const ready = email.trim() && address1.trim() && city.trim() && region.trim() && postal.trim()
    && (check || (account.trim() && routing.trim()));
  return <Dialog isOpen onOpenChange={o => !o && close()} width={520} purpose="form">
    <DialogHeader title="Enter bank details" subtitle={vendor.name} onOpenChange={o => !o && close()} />
    <VStack gap={4} padding={4}>
      {plan ? <VStack gap={2}>
        <Text>Mercury will add {vendor.name} as a recipient, paid by {METHOD[plan.input.method]} to {plan.destination}.</Text>
        <Text type="supporting">It then waits in Waiting on you. Before accepting it, confirm the details with the vendor by phone, on a number you already had: changed bank details are the most common way payments get stolen.</Text>
      </VStack> : <>
        <Text type="supporting">Inviting the vendor keeps their numbers out of Payvy entirely. Use this when they've already given you their details.</Text>
        <Selector label="Payment method" value={method} onChange={v => setMethod(v as Input['method'])} isDisabled={busy}
          options={['ach', 'domestic_wire', 'real_time_payment', 'check'].map(value => ({ value, label: METHOD[value] }))} />
        <TextInput label="Vendor email" type="email" value={email} onChange={setEmail} isRequired isDisabled={busy} />
        {!check && <>
          <TextInput label="Routing number" value={routing} onChange={setRouting} isRequired isDisabled={busy} />
          <TextInput label="Account number" value={account} onChange={setAccount} isRequired isDisabled={busy} />
        </>}
        {method === 'ach' && <Selector label="Account type" value={accountType} onChange={v => setAccountType(v as typeof accountType)} isDisabled={busy}
          options={Object.entries(ACCOUNT_TYPE).map(([value, label]) => ({ value, label }))} />}
        <TextInput label={check ? 'Mailing address' : 'Legal address'} value={address1} onChange={setAddress1} isRequired isDisabled={busy} />
        <TextInput label="Address line 2" value={address2} onChange={setAddress2} isDisabled={busy} />
        <HStack gap={2} wrap="wrap">
          <TextInput label="City" value={city} onChange={setCity} isRequired isDisabled={busy} />
          <TextInput label="State" value={region} onChange={setRegion} isRequired isDisabled={busy} />
          <TextInput label="ZIP" value={postal} onChange={setPostal} isRequired isDisabled={busy} />
        </HStack>
      </>}
      {error && <Banner status="error" title={error} />}
      <HStack gap={2} hAlign="end">
        <Button label={plan ? 'Back' : 'Cancel'} variant="ghost" isDisabled={busy} onClick={() => plan ? setPlan(null) : close()} />
        <Button label={plan ? 'Create in Mercury' : 'Review'} variant="primary" isDisabled={busy || !ready} clickAction={plan ? create : review} />
      </HStack>
    </VStack>
  </Dialog>;
}
