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

interface Subscription {
  subscribed: boolean;
  plan?: PlanName | null;
  status?: string | null;
  trial_end?: string | null;
  current_period_end?: string | null;
  cancel_at_period_end?: boolean | null;
  past_due_since?: string | null;
}

interface Billing {
  plan: { provisional?: boolean; reader: boolean; payments: boolean };
  subscription: Subscription;
}

const PLANS: { id: PlanName; name: string; price: string; points: string[] }[] = [
  {
    id: 'byoa',
    name: 'Bring your own agent',
    price: '$39 a month',
    points: [
      'Your agent reads invoices over MCP or the API, with your own model key.',
      'Approvals, workflows, and payments through your bank.',
    ],
  },
  {
    id: 'managed',
    name: 'Payvy reads for you',
    price: '$99 a month',
    points: [
      'Payvy reads every invoice that arrives and drafts the bill.',
      'Everything in Bring your own agent.',
    ],
  },
];

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

/** The past-due grace in the management app's plan policy. */
const GRACE_DAYS = 7;

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
      const r = await run<{ kind: string; url: string }>(command, input);
      window.location.assign(r.result.url);
    } catch (e) {
      setOpenError(message(e));
      setBusy(null);
    }
  }

  const sub = billing?.subscription;
  const live = !!sub?.subscribed && ['trialing', 'active', 'past_due'].includes(sub.status ?? '');
  const current = PLANS.find((p) => p.id === sub?.plan);
  const status = sub?.status ? STATUS[sub.status] : undefined;
  const graceEnds = sub?.past_due_since
    ? new Date(new Date(sub.past_due_since).getTime() + GRACE_DAYS * 86_400_000).toISOString()
    : null;

  return (
    <Page title="Billing" narrow description="Your Payvy plan. Payments go through Stripe; Payvy never sees your card.">
      {error && <Banner status="error" title="Couldn't load billing" description={error} />}
      {openError && <Banner status="error" title="Couldn't open billing" description={openError} />}
      {!billing && !error && <Spinner label="Loading" />}
      {billing && live && current && (
        <Card>
          <VStack gap={3}>
            <HStack gap={2} align="center">
              <Heading level={2}>{current.name}</Heading>
              {status && <Badge label={status.label} variant={status.variant} />}
            </HStack>
            <MetadataList>
              <MetadataListItem label="Price">{current.price}</MetadataListItem>
              {sub?.status === 'trialing' && sub.trial_end && (
                <MetadataListItem label="Trial ends">{date(sub.trial_end)}</MetadataListItem>
              )}
              {sub?.current_period_end && (
                <MetadataListItem label={sub.cancel_at_period_end ? 'Ends' : 'Renews'}>
                  {date(sub.current_period_end)}
                </MetadataListItem>
              )}
            </MetadataList>
            {sub?.status === 'past_due' && graceEnds && (
              <Banner
                status="warning"
                title="Your last payment failed"
                description={`Update your card by ${date(graceEnds)}, or new payments through your bank stop then. You can still sign in, read and export everything.`}
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
      {billing && !live && (
        <VStack gap={4}>
          {billing.plan.provisional && (
            <Banner
              status="info"
              title="Choose a plan to start"
              description="Until you do, Payvy won't read invoices or send payments. Both plans start with a 14-day free trial; you won't be charged until it ends, and you can cancel any time."
            />
          )}
          {!billing.plan.provisional && !billing.plan.payments && (
            <Banner
              status="warning"
              title="Your plan has ended"
              description="Payments and reading are off. You can still sign in, read and export everything. Choose a plan to turn them back on."
            />
          )}
          {PLANS.map((p) => (
            <Card key={p.id}>
              <VStack gap={2}>
                <HStack gap={2} align="center">
                  <Heading level={2}>{p.name}</Heading>
                  <Text type="supporting">{p.price}</Text>
                </HStack>
                {p.points.map((pt) => (
                  <Text key={pt}>{pt}</Text>
                ))}
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
