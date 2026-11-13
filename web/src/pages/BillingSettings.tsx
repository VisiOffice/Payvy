// Settings > Billing (hosted Payvy only). The plan is paid through Stripe:
// billing.checkout opens Checkout for a company without a subscription and
// the billing portal for one with it. Opening either grants nothing; the plan
// changes when Stripe confirms, and this page shows what is in force.
import { useEffect, useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { date } from '../money';
import { Page } from '../Page';

type PlanName = 'byoa' | 'managed';

// Names, prices and terms come from the management app (billing.get); this
// open-source page states none of its own.
interface Subscription {
  plan: PlanName | null;
  status: string;
  trial_end: string | null;
  current_period_end: string | null;
  cancel_at_period_end: boolean;
  payments_stop_at: string | null;
}

interface Offer {
  id: PlanName;
  name: string;
  price: string;
  description: string[];
  trial_days: number;
}

interface Billing {
  plan: { provisional?: boolean; reader: boolean; payments: boolean };
  available: boolean;
  subscription: Subscription | null;
  offers: Offer[];
}

const STATUS: Record<string, { label: string; variant: 'success' | 'warning' | 'error' | 'neutral' }> = {
  trialing: { label: 'Trial', variant: 'success' },
  active: { label: 'Active', variant: 'success' },
  past_due: { label: 'Payment failed', variant: 'warning' },
  unpaid: { label: 'Unpaid', variant: 'error' },
  canceled: { label: 'Canceled', variant: 'neutral' },
  incomplete: { label: 'Waiting for payment', variant: 'warning' },
  incomplete_expired: { label: 'Expired', variant: 'neutral' },
  paused: { label: 'Paused', variant: 'neutral' },
};

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function BillingSettings() {
  const [billing, setBilling] = useState<Billing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [openError, setOpenError] = useState<string | null>(null);

  useEffect(() => {
    run<Billing>('billing.get', {})
      .then((r) => setBilling(r.result))
      .catch((e) => setError(message(e)));
  }, []);

  async function open(command: 'billing.checkout' | 'billing.portal', input: Record<string, unknown>, key: string) {
    setBusy(key);
    setOpenError(null);
    try {
      const r = await run<{ kind: string; url: string | null }>(command, input);
      const url = r.result.url;
      if (!url?.startsWith('https://')) throw new Error('Billing answered without a usable link; try again.');
      window.location.assign(url);
    } catch (e) {
      setOpenError(message(e));
      setBusy(null);
    }
  }

  const sub = billing?.subscription;
  const offers = billing?.offers ?? [];
  const live = !!sub && ['trialing', 'active', 'past_due'].includes(sub.status);
  const current = offers.find((p) => p.id === sub?.plan);
  const status = sub ? STATUS[sub.status] : undefined;
  const trialDays = Math.max(0, ...offers.map((o) => o.trial_days));

  return (
    <Page title="Billing" narrow description="Your Payvy plan. Payments go through Stripe; Payvy never sees your card.">
      {error && <Banner status="error" title="Couldn't load billing" description={error} />}
      {openError && <Banner status="error" title="Couldn't open billing" description={openError} />}
      {!billing && !error && <Spinner label="Loading" />}
      {billing && !billing.available && (
        <VStack gap={3}>
          <Banner
            status="warning"
            title="Billing can't be reached right now"
            description="Your plan and card details will be back shortly. Nothing about your plan has changed."
          />
          <Card>
            <MetadataList>
              <MetadataListItem label="Payments through your bank">{billing.plan.payments ? 'On' : 'Off'}</MetadataListItem>
              <MetadataListItem label="Payvy reads invoices">{billing.plan.reader ? 'On' : 'Off'}</MetadataListItem>
            </MetadataList>
          </Card>
        </VStack>
      )}
      {billing && billing.available && live && sub && (
        <Card>
          <VStack gap={3}>
            <HStack gap={2} align="center">
              <Heading level={2}>{current?.name ?? 'Your plan'}</Heading>
              {status && <Badge label={status.label} variant={status.variant} />}
            </HStack>
            <MetadataList>
              {current && <MetadataListItem label="Price">{current.price}</MetadataListItem>}
              {sub.status === 'trialing' && sub.trial_end && (
                <MetadataListItem label="Trial ends">{date(sub.trial_end)}</MetadataListItem>
              )}
              {sub.current_period_end && (
                <MetadataListItem label={sub.cancel_at_period_end ? 'Ends' : 'Renews'}>
                  {date(sub.current_period_end)}
                </MetadataListItem>
              )}
            </MetadataList>
            {sub.status === 'past_due' && (
              <Banner
                status="warning"
                title="Your last payment failed"
                description={
                  sub.payments_stop_at
                    ? `Update your card by ${date(sub.payments_stop_at)}, or new payments through your bank stop then. You can still sign in, read and export everything.`
                    : 'Update your card, or new payments through your bank will stop. You can still sign in, read and export everything.'
                }
              />
            )}
            <HStack>
              <Button
                label="Manage billing"
                variant="primary"
                isLoading={busy === 'portal'}
                onClick={() => void open('billing.portal', {}, 'portal')}
              />
            </HStack>
            <Text type="supporting">Change plan or card, see invoices, or cancel, in Stripe.</Text>
          </VStack>
        </Card>
      )}
      {billing && billing.available && !live && (
        <VStack gap={4}>
          {billing.plan.provisional && (
            <Banner
              status="info"
              title="Choose a plan to start"
              description={
                trialDays > 0
                  ? `Until you do, Payvy won't read invoices or send payments. Plans start with a ${trialDays}-day free trial; you won't be charged until it ends, and you can cancel any time.`
                  : "Until you do, Payvy won't read invoices or send payments."
              }
            />
          )}
          {!billing.plan.provisional && !billing.plan.payments && (
            <Banner
              status="warning"
              title="Your plan has ended"
              description="Payments and reading are off. You can still sign in, read and export everything. Choose a plan to turn them back on."
            />
          )}
          {offers.map((p) => (
            <Card key={p.id}>
              <VStack gap={2}>
                <HStack gap={2} align="center">
                  <Heading level={2}>{p.name}</Heading>
                  <Text type="supporting">{p.price}</Text>
                </HStack>
                {p.description.map((line) => (
                  <Text key={line}>{line}</Text>
                ))}
                {p.trial_days > 0 && <Text type="supporting">{`${p.trial_days}-day free trial`}</Text>}
                <HStack>
                  <Button
                    label={`Choose ${p.name}`}
                    variant={p.id === 'byoa' ? 'secondary' : 'primary'}
                    isLoading={busy === p.id}
                    isDisabled={!!busy && busy !== p.id}
                    onClick={() => void open('billing.checkout', { plan: p.id }, p.id)}
                  />
                </HStack>
              </VStack>
            </Card>
          ))}
        </VStack>
      )}
    </Page>
  );
}
