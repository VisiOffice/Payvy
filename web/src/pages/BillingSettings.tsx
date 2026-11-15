// Settings > Billing (hosted Payvy only). The plan is paid through Stripe:
// billing.checkout opens Checkout for a company without a subscription and
// the billing portal for one with it. Opening either grants nothing; the plan
// changes when Stripe confirms, and this page shows what is in force. A new
// company starts in a trial the management app gives it, with no card: the
// page says when it ends, or that it has.
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
import { isLive, openBilling, PlanCards, type Billing } from '../billing';
import { date } from '../money';
import { Page } from '../Page';

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
  const [busy, setBusy] = useState(false);
  const [openError, setOpenError] = useState<string | null>(null);

  useEffect(() => {
    run<Billing>('billing.get', {})
      .then((r) => setBilling(r.result))
      .catch((e) => setError(message(e)));
  }, []);

  async function portal() {
    setBusy(true);
    setOpenError(null);
    try {
      await openBilling();
    } catch (e) {
      setOpenError(message(e));
      setBusy(false);
    }
  }

  const sub = billing?.subscription;
  const offers = billing?.offers ?? [];
  const live = isLive(sub);
  const current = offers.find((p) => p.id === sub?.plan);
  const status = sub ? STATUS[sub.status] : undefined;
  const trial = billing?.trial ?? null;
  // Over, and payments are off. (Not for a company whose payments were
  // turned on by hand, or in the hour before the plan catches up.)
  const trialEnded = !!trial && !trial.running && !billing?.plan.payments;

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
                isLoading={busy}
                onClick={() => void portal()}
              />
            </HStack>
            <Text type="supporting">Change plan or card, see invoices, or cancel, in Stripe.</Text>
          </VStack>
        </Card>
      )}
      {billing && billing.available && !live && (
        <VStack gap={4}>
          {trial?.running && (
            <Banner
              status="info"
              title={`Trial until ${date(trial.ends_at)}.`}
              description="Add a plan to keep paying bills after that."
            />
          )}
          {trial && trialEnded && (
            <Banner
              status="warning"
              title={`Your trial ended on ${date(trial.ends_at)}.`}
              description="Choose a plan to send payments again."
            />
          )}
          {!trial && billing.plan.provisional && (
            <Banner
              status="info"
              title="Choose a plan to start"
              description="Until you do, Payvy won't read invoices or send payments."
            />
          )}
          {!trial && !billing.plan.provisional && !billing.plan.payments && (
            <Banner
              status="warning"
              title="Your plan has ended"
              description="Payments and reading are off. You can still sign in, read and export everything. Choose a plan to turn them back on."
            />
          )}
          <PlanCards offers={offers} onError={setOpenError} />
        </VStack>
      )}
    </Page>
  );
}
