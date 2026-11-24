// A vendor that keeps one of the company's cards and charges it: which card,
// and the charges Payvy has seen on it. Linking, matching and dismissing are
// a person's (vendor.link_card, card.match_charge, card.dismiss_charge).
import type { ComponentProps } from 'react';
import { useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { DateInput } from '@astryxdesign/core/DateInput';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Selector } from '@astryxdesign/core/Selector';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import type { Listed } from '../bills/BillsTable';
import { date, money } from '../money';
import { Allowed } from '../permissions';

export interface CardOnFile {
  id: string;
  last_four: string;
  nickname: string | null;
  kind: string;
  charges_from: string;
  linked_by: string;
  linked_at: string;
}

export interface CardCharge {
  id: string;
  transaction_id: string;
  amount_cents: number;
  status: 'pending' | 'posted' | 'reversed';
  counterparty: string | null;
  charged_at: string;
  bill_id: string | null;
  invoice_number: string | null;
  bill_state: string | null;
  rule: string | null;
  dismissed_reason: string | null;
  link: string | null;
  needs?: string | null;
}

interface BankCard {
  id: string;
  last_four: string;
  nickname: string | null;
  kind: string;
  status: string;
  on_file_with?: { vendor_id: string; vendor_name: string | null };
}

type ISODate = NonNullable<ComponentProps<typeof DateInput>['value']>;
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
const label = (c: { last_four: string; nickname: string | null; kind: string }) =>
  `••${c.last_four}${c.nickname ? ` · ${c.nickname}` : ''} · ${c.kind}`;

export function VendorCardOnFile({
  vendorId,
  vendorName,
  card,
  charges,
  bills,
  onChanged,
}: {
  vendorId: string;
  vendorName: string;
  card: CardOnFile | null;
  charges: CardCharge[];
  bills: Listed[] | null;
  onChanged: () => Promise<void>;
}) {
  const [choosing, setChoosing] = useState(false);
  const [cards, setCards] = useState<BankCard[] | null>(null);
  const [chosen, setChosen] = useState<string | undefined>();
  const [from, setFrom] = useState<string>('');
  const [removing, setRemoving] = useState(false);
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function act(f: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await f();
      await onChanged();
      return true;
    } catch (e) {
      setError(message(e));
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function openChooser() {
    setChoosing(true);
    setChosen(undefined);
    setError(null);
    try {
      const r = await run<{ cards: BankCard[] }>('bank.cards', {});
      setCards(r.result.cards);
    } catch (e) {
      setError(message(e));
    }
  }

  const usable = (cards ?? []).filter(
    (c) => ['active', 'frozen'].includes(c.status) && (!c.on_file_with || c.on_file_with.vendor_id === vendorId) && c.id !== card?.id,
  );

  return (
    <VStack gap={3}>
      {card ? (
        <MetadataList>
          <MetadataListItem label="Card">{label(card)}</MetadataListItem>
          <MetadataListItem label="Charges matched from">{date(card.charges_from)}</MetadataListItem>
          <MetadataListItem label="Linked by">
            {card.linked_by} on {date(card.linked_at)}
          </MetadataListItem>
        </MetadataList>
      ) : (
        <Text type="supporting">
          None. If {vendorName} keeps one of your cards and charges it, link that card: releasing its bills then sends no
          payment, and Payvy marks a bill paid when a charge plainly pays it.
        </Text>
      )}
      {card && (
        <Text type="supporting">
          Releasing a bill sends no payment: {vendorName} charges the card, and Payvy marks the bill paid when the charge
          posts.
        </Text>
      )}

      {!choosing && !removing && (
        <HStack gap={2} wrap="wrap">
          <Allowed command="vendor.link_card">
            <Button label={card ? 'Change card…' : 'Link a card…'} variant="secondary" size="sm" isDisabled={busy} onClick={openChooser} />
          </Allowed>
          {card && (
            <Allowed command="vendor.unlink_card">
              <Button label="Remove card" variant="ghost" size="sm" isDisabled={busy} onClick={() => { setRemoving(true); setReason(''); }} />
            </Allowed>
          )}
        </HStack>
      )}

      {choosing && (
        <VStack gap={2}>
          {!cards && !error && <Text type="supporting">Asking the bank for your cards…</Text>}
          {cards && usable.length === 0 && <Text>No other active card is free to link. A card can be on file with one vendor only.</Text>}
          {usable.length > 0 && (
            <Selector
              label="Card at the bank"
              placeholder="Choose the card this vendor keeps"
              value={chosen}
              onChange={setChosen}
              isDisabled={busy}
              options={usable.map((c) => ({ value: c.id, label: `${label(c)}${c.status === 'frozen' ? ' · frozen' : ''}` }))}
            />
          )}
          <DateInput
            label="Match charges from"
            format="date"
            value={/^\d{4}-\d{2}-\d{2}$/.test(from) ? (from as ISODate) : undefined}
            onChange={(v) => setFrom(v ?? '')}
          />
          <Text type="supporting">Leave empty to start 30 days back. Earlier charges are left alone.</Text>
          <HStack gap={2}>
            <Button
              label="Link card"
              isDisabled={busy || !chosen}
              clickAction={async () => {
                const ok = await act(() =>
                  run('vendor.link_card', { vendor_id: vendorId, card_id: chosen!, ...(from ? { charges_from: from } : {}) }),
                );
                if (ok) setChoosing(false);
              }}
            />
            <Button label="Cancel" variant="ghost" isDisabled={busy} onClick={() => setChoosing(false)} />
          </HStack>
        </VStack>
      )}

      {removing && card && (
        <VStack gap={2}>
          <TextInput label="Why remove it" value={reason} onChange={setReason} placeholder="e.g. they now take ACH" isDisabled={busy} />
          <HStack gap={2}>
            <Button
              label="Remove card"
              variant="destructive"
              isDisabled={busy || !reason.trim()}
              clickAction={async () => {
                const ok = await act(() => run('vendor.unlink_card', { vendor_id: vendorId, reason: reason.trim() }));
                if (ok) setRemoving(false);
              }}
            />
            <Button label="Cancel" variant="ghost" isDisabled={busy} onClick={() => setRemoving(false)} />
          </HStack>
        </VStack>
      )}

      {error && <Banner status="error" title={error} />}

      {charges.length > 0 && (
        <VStack gap={2}>
          <Text weight="semibold">Charges</Text>
          {charges.map((c) => (
            <Charge key={c.id} charge={c} bills={bills} busy={busy} act={act} />
          ))}
        </VStack>
      )}
    </VStack>
  );
}

function Charge({
  charge: c,
  bills,
  busy,
  act,
}: {
  charge: CardCharge;
  bills: Listed[] | null;
  busy: boolean;
  act: (f: () => Promise<unknown>) => Promise<boolean>;
}) {
  const [mode, setMode] = useState<'match' | 'dismiss' | null>(null);
  const [bill, setBill] = useState<string | undefined>();
  const [reason, setReason] = useState('');
  const candidates = (bills ?? []).filter((b) => b.bill.state === 'open' && b.bill.amount_cents === c.amount_cents);
  const open = !c.bill_id && !c.dismissed_reason && c.status === 'posted';

  return (
    <VStack gap={1}>
      <HStack gap={2} vAlign="center" wrap="wrap">
        <Text>
          {date(c.charged_at)} · {money(c.amount_cents)}
        </Text>
        {c.status === 'pending' && <Badge variant="neutral" label="Pending" />}
        {c.status === 'reversed' && <Badge variant="neutral" label="Reversed" />}
        {c.dismissed_reason && <Badge variant="neutral" label={`Dismissed: ${c.dismissed_reason}`} />}
        {c.bill_id && (
          <Text>
            pays <Link href={`/bills/${c.bill_id}`}>{c.invoice_number ?? 'a bill'}</Link>
            {c.rule === 'amount' ? ' (matched by amount)' : ''}
          </Text>
        )}
        {c.needs && <Badge variant="warning" label="Needs a person" />}
        {c.link && <Link href={c.link}>In Mercury</Link>}
      </HStack>
      {c.needs && <Text type="supporting">{c.needs}</Text>}
      {open && !mode && (
        <HStack gap={2}>
          <Allowed command="card.match_charge">
            <Button label="Match to a bill…" variant="secondary" size="sm" isDisabled={busy} onClick={() => setMode('match')} />
          </Allowed>
          <Allowed command="card.dismiss_charge">
            <Button label="Dismiss…" variant="ghost" size="sm" isDisabled={busy} onClick={() => setMode('dismiss')} />
          </Allowed>
        </HStack>
      )}
      {mode === 'match' && (
        <VStack gap={2}>
          {candidates.length === 0 ? (
            <Text>No open bill of {money(c.amount_cents)}. A charge pays one whole bill; correct the bill's amount first if the vendor charged differently.</Text>
          ) : (
            <Selector
              label="Bill this charge paid"
              placeholder="Choose a bill"
              value={bill}
              onChange={setBill}
              options={candidates.map((b) => ({ value: b.bill.id, label: `${b.bill.invoice_number} · ${money(b.bill.amount_cents)}` }))}
            />
          )}
          <HStack gap={2}>
            <Button
              label="Match"
              isDisabled={busy || !bill}
              clickAction={async () => {
                if (await act(() => run('card.match_charge', { charge_id: c.id, bill_id: bill! }))) setMode(null);
              }}
            />
            <Button label="Cancel" variant="ghost" onClick={() => setMode(null)} />
          </HStack>
        </VStack>
      )}
      {mode === 'dismiss' && (
        <VStack gap={2}>
          <TextInput label="Why it pays no bill" value={reason} onChange={setReason} placeholder="e.g. late fee, refunded" />
          <HStack gap={2}>
            <Button
              label="Dismiss"
              isDisabled={busy || !reason.trim()}
              clickAction={async () => {
                if (await act(() => run('card.dismiss_charge', { charge_id: c.id, reason: reason.trim() }))) setMode(null);
              }}
            />
            <Button label="Cancel" variant="ghost" onClick={() => setMode(null)} />
          </HStack>
        </VStack>
      )}
    </VStack>
  );
}
