import { useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Dialog, DialogHeader } from '@astryxdesign/core/Dialog';
import { HStack } from '@astryxdesign/core/HStack';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { ApiError, preview, run } from '../api/client';
import { money } from '../money';
import { METHOD, type Bill } from './types';

interface Plan {
  bank: string;
  from_account: string;
  mode: string;
  to: string;
  destination: string;
  method: string;
  amount_cents: number;
  then: string;
}

type State =
  | { kind: 'loading' }
  | { kind: 'ready'; token: string; plan: Plan | null }
  | { kind: 'needs_payee'; message: string }
  | { kind: 'error'; message: string };

/**
 * Releasing moves money, so it shows exactly what will be sent (the server's
 * preview) and commits only that, with the preview's one-use token.
 */
export function ReleaseDialog({
  bill,
  onClose,
  onReleased,
}: {
  bill: Bill;
  onClose: () => void;
  onReleased: (message: string) => void;
}) {
  const [payElsewhere, setPayElsewhere] = useState(false);
  const [state, setState] = useState<State>({ kind: 'loading' });
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setState({ kind: 'loading' });
    preview<{ payment_plan?: Plan }>('bill.release', {
      bill_id: bill.id,
      ...(payElsewhere ? { pay_elsewhere: true } : {}),
    })
      .then((p) =>
        setState({ kind: 'ready', token: p.confirm_token!, plan: p.result.payment_plan ?? null }),
      )
      .catch((e: unknown) => {
        const message = e instanceof Error ? e.message : String(e);
        if (e instanceof ApiError && e.code === 'conflict' && message.includes('payee')) {
          setState({ kind: 'needs_payee', message });
        } else {
          setState({ kind: 'error', message });
        }
      });
  }, [bill.id, payElsewhere]);

  async function release() {
    if (state.kind !== 'ready') return;
    setError(null);
    try {
      const r = await run<{ payments?: { status: string }[] }>(
        'bill.release',
        { bill_id: bill.id, ...(payElsewhere ? { pay_elsewhere: true } : {}) },
        state.token,
      );
      const status = r.result.payments?.[0]?.status;
      onReleased(
        status === 'awaiting_bank_approval'
          ? `Sent to Mercury for approval: ${money(bill.amount_cents)} to ${bill.vendor_name}`
          : status === 'failed'
            ? `The bank refused the payment to ${bill.vendor_name}; it's back in your inbox`
            : state.plan
              ? `Paying ${money(bill.amount_cents)} to ${bill.vendor_name}`
              : `Released. Mark it paid once you've paid it.`,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <Dialog isOpen onOpenChange={(open) => !open && onClose()} width={480} purpose="form">
      <DialogHeader
        title={`Release ${money(bill.amount_cents)} to ${bill.vendor_name}`}
        subtitle={`Invoice ${bill.invoice_number}`}
        onOpenChange={(open) => !open && onClose()}
      />
      <VStack gap={4} padding={4}>
        {state.kind === 'loading' && <Spinner label="Checking what will be sent" />}
        {state.kind === 'error' && <Banner status="error" title={state.message} />}
        {state.kind === 'needs_payee' && (
          <VStack gap={3}>
            <Banner
              status="warning"
              title="This vendor has no bank details in Payvy yet"
              description="Link them to a recipient in Mercury first, or release it and pay it another way."
            />
            <HStack gap={2} hAlign="end">
              <Button label="Cancel" variant="ghost" onClick={onClose} />
              <Button label="Pay it another way" onClick={() => setPayElsewhere(true)} />
            </HStack>
          </VStack>
        )}
        {state.kind === 'ready' && (
          <VStack gap={4}>
            {state.plan ? (
              <MetadataList>
                <MetadataListItem label="Amount">{money(state.plan.amount_cents)}</MetadataListItem>
                <MetadataListItem label="To">{state.plan.to}</MetadataListItem>
                <MetadataListItem label="Account">{state.plan.destination}</MetadataListItem>
                <MetadataListItem label="Method">
                  {METHOD[state.plan.method] ?? state.plan.method}
                </MetadataListItem>
                <MetadataListItem label="From">{state.plan.from_account}</MetadataListItem>
                <MetadataListItem label="Then">{state.plan.then}</MetadataListItem>
              </MetadataList>
            ) : (
              <Text>
                Payvy won't send this payment. Pay it however you normally do, then mark it paid.
              </Text>
            )}
            {error && <Banner status="error" title={error} />}
            <HStack gap={2} hAlign="end">
              <Button label="Cancel" variant="ghost" onClick={onClose} />
              <Button
                label={state.plan ? `Release ${money(state.plan.amount_cents)}` : 'Release'}
                variant="primary"
                clickAction={release}
              />
            </HStack>
          </VStack>
        )}
      </VStack>
    </Dialog>
  );
}
