import { useEffect, useState } from 'react';
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

export interface PayeeInvite {
  id: string;
  connection_id: string;
  contact_email: string;
  method: string;
  status: string;
  mercury_invite_id: string | null;
  proposal_id: string | null;
}
const LABELS: Record<string, string> = {
  created: 'Waiting for the vendor', completed: 'Vendor completed the invitation',
  expired: 'Invitation expired', declined: 'Vendor declined',
  unknown: 'Invitation delivery uncertain', failed: 'Mercury refused the invitation',
};
const message = (e: unknown) => e instanceof Error ? e.message : String(e);

export function PayeeInvites({ vendor, invites, payees, onChanged }: {
  vendor: { id: string; name: string; email: string | null };
  invites: PayeeInvite[];
  payees: { id: string; status: string }[];
  onChanged: () => Promise<void>;
}) {
  const [connection, setConnection] = useState<{ id: string; provider: string } | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    run<{ connection: { id: string; provider: string } | null }>('bank.get', {})
      .then(r => { if (active) { setConnection(r.result.connection); setLoaded(true); } })
      .catch(e => { if (active) setError(message(e)); });
    return () => { active = false; };
  }, [vendor.id]);
  const current = invites.filter(i => i.connection_id === connection?.id);
  const pending = current.some(i => ['created', 'unknown'].includes(i.status) || (i.status === 'completed' && !i.proposal_id));
  return <VStack gap={3}>
    {error && <Banner status="error" title={error} />}
    {current.map(invite => <InviteStatus key={invite.id} invite={invite} proposalStatus={payees.find(p => p.id === invite.proposal_id)?.status} onChanged={onChanged} />)}
    {loaded && connection?.provider !== 'mercury' && <Text type="supporting">Connect Mercury to invite this vendor to supply payment details.</Text>}
    {connection?.provider === 'mercury' && !pending && <Button label="Invite payment details…" variant="secondary" onClick={() => setOpen(true)} />}
    {open && <InviteDialog vendor={vendor} onClose={() => setOpen(false)} onChanged={onChanged} />}
  </VStack>;
}

function InviteStatus({ invite, proposalStatus, onChanged }: { invite: PayeeInvite; proposalStatus?: string; onChanged: () => Promise<void> }) {
  const [recoveryId, setRecoveryId] = useState('');
  const [error, setError] = useState<string | null>(null);
  const unresolved = ['created', 'unknown'].includes(invite.status) || (invite.status === 'completed' && !invite.proposal_id);
  async function refresh() {
    setError(null);
    try {
      await run('vendor.refresh_payee_invite', { invite_id: invite.id,
        ...(invite.status === 'unknown' && !invite.mercury_invite_id ? { mercury_invite_id: recoveryId.trim() } : {}) });
      await onChanged();
    } catch (e) { setError(message(e)); }
  }
  return <VStack gap={2}>
    <Text weight="semibold">{proposalStatus === 'active' ? 'Payment details accepted' : LABELS[invite.status] ?? invite.status}</Text>
    <Text type="supporting">{invite.contact_email} · {METHOD[invite.method] ?? invite.method}</Text>
    {invite.status === 'unknown' && <>
      <Text>Check Mercury before sending another invitation. If it exists, enter its invitation ID to check it here.</Text>
      {!invite.mercury_invite_id && <TextInput label="Mercury invitation ID" value={recoveryId} onChange={setRecoveryId} />}
    </>}
    {proposalStatus === 'proposed' && <Text type="supporting">Review the proposed destination in Waiting on you before paying this vendor.</Text>}
    {error && <Banner status="warning" title={error} />}
    {unresolved && <Button label="Check invitation" variant="secondary" clickAction={refresh}
      isDisabled={invite.status === 'unknown' && !invite.mercury_invite_id && !recoveryId.trim()} />}
  </VStack>;
}

function InviteDialog({ vendor, onClose, onChanged }: {
  vendor: { id: string; name: string; email: string | null };
  onClose: () => void;
  onChanged: () => Promise<void>;
}) {
  const [email, setEmail] = useState(vendor.email ?? '');
  const [method, setMethod] = useState<CommandInputs['vendor.invite_payee']['method']>('ach');
  const [plan, setPlan] = useState<{ token: string; input: CommandInputs['vendor.invite_payee']; name: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  async function review() {
    setBusy(true); setError(null);
    try {
      const input = { vendor_id: vendor.id, email: email.trim(), method };
      const p = await preview<{ vendor_name: string }>('vendor.invite_payee', input);
      if (!p.confirm_token) throw new Error('Could not prepare the invitation. Try again.');
      setPlan({ token: p.confirm_token, input, name: p.result.vendor_name });
    } catch (e) { setError(message(e)); } finally { setBusy(false); }
  }
  async function send() {
    if (!plan) return;
    setBusy(true); setError(null);
    try {
      await run('vendor.invite_payee', plan.input, plan.token);
      await onChanged(); onClose();
    } catch (e) {
      setError(message(e)); setPlan(null);
      // A lost response may still have created the durable attempt.
      await onChanged().catch(() => {});
    } finally { setBusy(false); }
  }
  const close = () => { if (!busy) onClose(); };
  return <Dialog isOpen onOpenChange={o => !o && close()} width={460} purpose="form">
    <DialogHeader title="Invite payment details" subtitle={vendor.name} onOpenChange={o => !o && close()} />
    <VStack gap={4} padding={4}>
      {plan ? <Text>Mercury will email {plan.input.email} to collect {METHOD[plan.input.method]} details for {plan.name}. You will review and accept the destination before it can be used for payment.</Text> : <>
        <Text>The vendor enters their payment details directly in Mercury. Payvy keeps only the recipient reference and a masked destination.</Text>
        <TextInput label="Vendor email" type="email" value={email} onChange={setEmail} isRequired isDisabled={busy} />
        <Selector label="Payment method" value={method} onChange={v => setMethod(v as typeof method)} isDisabled={busy}
          options={['ach', 'check', 'domestic_wire', 'real_time_payment'].map(value => ({ value, label: METHOD[value] }))} />
      </>}
      {error && <Banner status="error" title={error} />}
      <HStack gap={2} hAlign="end">
        <Button label={plan ? 'Back' : 'Cancel'} variant="ghost" isDisabled={busy} onClick={() => plan ? setPlan(null) : close()} />
        <Button label={plan ? 'Send invitation' : 'Review invitation'} variant="primary" isDisabled={busy || !email.trim()} clickAction={plan ? send : review} />
      </HStack>
    </VStack>
  </Dialog>;
}
