import { useEffect, useRef, useState } from 'react';
import { AlertDialog } from '@astryxdesign/core/AlertDialog';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Selector } from '@astryxdesign/core/Selector';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { preview, run } from '../api/client';
import type { BankActivateInput } from '../api/commands.gen';

interface Choice { entity_id: string; entity_name: string; account_id: string; account_name: string; destination: string; fingerprint: string }
interface Stage {
  authorization_id: string;
  business: { business_name_legal: string };
  environment: string;
  choices: Choice[];
  replacing: { id: string; account_name: string; provider: string } | null;
}
const message = (e: unknown) => e instanceof Error ? e.message : String(e);
const KEY = 'payvy:ramp-authorization'; // Identifier only; the server binds it to person + company.

export function RampConnection({ available, onChanged }: { available: boolean; onChanged: () => void }) {
  const started = useRef(false);
  const [stage, setStage] = useState<Stage | null>(null);
  const [selected, setSelected] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [plan, setPlan] = useState<{ input: BankActivateInput; token: string; description: string } | null>(null);
  async function load(id: string) {
    const r = await run<Stage>('bank.authorization', { authorization_id: id });
    setStage(r.result);
    setSelected(undefined);
  }
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    const params = new URLSearchParams(window.location.search);
    const code = params.get('code');
    const state = params.get('state');
    const denied = params.has('error');
    // Remove OAuth parameters from the visible URL before making any more requests.
    if (code || state || denied) window.history.replaceState(window.history.state, '', window.location.pathname);
    if (denied) { sessionStorage.removeItem(KEY); setError('Ramp authorization was not completed. You can start again.'); return; }
    if (!available) return;
    setBusy(true);
    (async () => {
      if (code && state) {
        const r = await run<{ authorization_id: string; status: string; error?: string }>('bank.complete_authorization', { code, state });
        if (r.result.status !== 'ready') throw new Error(r.result.error ?? 'Ramp authorization failed. Start again.');
        sessionStorage.setItem(KEY, r.result.authorization_id);
        await load(r.result.authorization_id);
      } else {
        const id = sessionStorage.getItem(KEY);
        if (id) await load(id);
      }
    })().catch(e => { sessionStorage.removeItem(KEY); setError(message(e)); }).finally(() => setBusy(false));
  }, [available]);

  async function begin() {
    setError(null); setBusy(true);
    try {
      const r = await run<{ authorize_url: string }>('bank.authorize', {});
      sessionStorage.removeItem(KEY);
      window.location.assign(r.result.authorize_url);
    } catch (e) { setError(message(e)); setBusy(false); }
  }
  async function review() {
    const choice = stage?.choices.find(c => `${c.entity_id}:${c.account_id}` === selected);
    if (!stage || !choice) return;
    setBusy(true); setError(null);
    const input: BankActivateInput = { authorization_id: stage.authorization_id, entity_id: choice.entity_id,
      account_id: choice.account_id, fingerprint: choice.fingerprint, replacing_connection_id: stage.replacing?.id ?? null };
    try {
      const p = await preview('bank.activate', input);
      if (!p.confirm_token) throw new Error('Could not prepare activation. Try again.');
      setPlan({ input, token: p.confirm_token, description: `${stage.business.business_name_legal} · ${stage.environment}. ${choice.entity_name} · ${choice.account_name} ${choice.destination}. ${stage.replacing ? `Replaces ${stage.replacing.provider}: ${stage.replacing.account_name}.` : 'No bank is currently selected.'} Ramp sending and auto-pay remain disabled. Existing vendor mappings must be proposed again. Payments already sent through Mercury continue to be tracked.` });
    } catch (e) { setError(message(e)); } finally { setBusy(false); }
  }
  return <VStack gap={3}>
    <Heading level={2}>Ramp</Heading>
    <Card><VStack gap={3}>
      <Text>Connect Ramp to choose a funding account and map your vendors to their existing ACH accounts.</Text>
      <Banner status="warning" title="Setup only — payments are not enabled" description="Authorizing leaves your current bank in place. Selecting Ramp replaces it after you review and confirm." />
      {!available && <Text type="supporting">Ramp is not configured on this server yet.</Text>}
      {error && <Banner status="error" title={error} />}
      {stage && <>
        <Text weight="semibold">{stage.business.business_name_legal} · {stage.environment}</Text>
        {stage.choices.length ? <Selector label="Ramp entity and funding account" placeholder="Choose an account" value={selected}
          onChange={v => { setSelected(v); setPlan(null); }} isDisabled={busy}
          options={stage.choices.map(c => ({ value: `${c.entity_id}:${c.account_id}`, label: `${c.entity_name} · ${c.account_name} ${c.destination}` }))} />
          : <Text>No eligible USD ACH funding accounts were returned. Check the entity and account setup in Ramp.</Text>}
        {stage.replacing && <Text>Currently selected: {stage.replacing.provider} · {stage.replacing.account_name}</Text>}
        <HStack gap={2}>
          <Button label="Review Ramp activation" isDisabled={busy || !selected} onClick={review} />
          <Button label="Cancel setup" variant="ghost" isDisabled={busy} onClick={() => { sessionStorage.removeItem(KEY); setStage(null); setPlan(null); }} />
        </HStack>
      </>}
      {available && <Button label={stage ? 'Start authorization again' : 'Authorize Ramp'} variant="secondary" isDisabled={busy} onClick={begin} />}
    </VStack></Card>
    <AlertDialog isOpen={!!plan} onOpenChange={open => { if (!open && !busy) setPlan(null); }} title="Select Ramp for setup?"
      description={plan?.description ?? ''} actionLabel="Select Ramp" isActionLoading={busy} onAction={async () => {
        if (!plan) return;
        setBusy(true); setError(null);
        try { await run('bank.activate', plan.input, plan.token); sessionStorage.removeItem(KEY); setPlan(null); setStage(null); onChanged(); }
        catch (e) { setPlan(null); setError(message(e)); }
        finally { setBusy(false); }
      }} />
  </VStack>;
}
