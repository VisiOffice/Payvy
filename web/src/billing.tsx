// What billing.get answers, and the plan cards built from it. Settings >
// Billing and the first-run checklist show the same cards. Names, prices and
// terms come from the management app; this open-source app states none of
// its own.
import { useState } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from './api/client';
import type { CommandInputs } from './api/commands.gen';

export type PlanName = 'byoa' | 'managed';

export interface Subscription {
  plan: PlanName | null;
  status: string;
  trial_end: string | null;
  current_period_end: string | null;
  cancel_at_period_end: boolean;
  payments_stop_at: string | null;
}

/** The price carries its terms, as the management app words them. */
export interface Offer {
  id: PlanName;
  name: string;
  price: string;
  description: string[];
}

/** The card-free trial a new company starts with. `running`: it is what
 *  lets the company pay bills now (not ended, and no subscription yet). */
export interface Trial {
  ends_at: string;
  running: boolean;
}

export interface Billing {
  plan: { provisional?: boolean; reader: boolean; payments: boolean };
  available: boolean;
  subscription: Subscription | null;
  /** Null for a company given no trial; absent from an older server. */
  trial?: Trial | null;
  offers: Offer[];
}

/** A subscription that is paying for a plan now, or about to. */
export function isLive(sub: Subscription | null | undefined): sub is Subscription {
  return !!sub && ['trialing', 'active', 'past_due'].includes(sub.status);
}

/** The plan that includes Payvy's own reading (docs/hosted.md). */
export const readerOffer = (offers: Offer[]) => offers.find((o) => o.id === 'managed');

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Opens Stripe: Checkout for a plan, or the portal with no plan. Leaves
 *  the app on success; throws with a sentence for people otherwise. */
export async function openBilling(checkout?: CommandInputs['billing.checkout']) {
  const r = checkout
    ? await run<{ url: string | null }>('billing.checkout', checkout)
    : await run<{ url: string | null }>('billing.portal', {});
  const url = r.result.url;
  if (!url?.startsWith('https://')) throw new Error('Billing answered without a usable link; try again.');
  window.location.assign(url);
}

/** One card per plan on offer, each with its button to Checkout. Shown only
 *  to people who may run billing.checkout. */
export function PlanCards({
  offers,
  returnTo,
  headingLevel = 2,
  isDisabled,
  onOpening,
  onError,
}: {
  offers: Offer[];
  /** Where Checkout comes back to; Settings > Billing when not given. */
  returnTo?: 'start';
  /** 3 where the cards sit under a heading of their own. */
  headingLevel?: 2 | 3;
  isDisabled?: boolean;
  /** Called just before the browser leaves for Checkout. */
  onOpening?: () => void;
  onError: (message: string | null) => void;
}) {
  const [busy, setBusy] = useState<PlanName | null>(null);

  async function choose(plan: PlanName) {
    setBusy(plan);
    onError(null);
    try {
      onOpening?.();
      // An older server knows no return_to, so it is sent only when asked for.
      await openBilling(returnTo ? { plan, return_to: returnTo } : { plan });
    } catch (e) {
      onError(message(e));
      setBusy(null);
    }
  }

  return (
    <>
      {offers.map((p) => (
        <Card key={p.id}>
          <VStack gap={2}>
            <HStack gap={2} align="center" wrap="wrap">
              <Heading level={headingLevel}>{p.name}</Heading>
              <Text type="supporting">{p.price}</Text>
            </HStack>
            {p.description.map((line) => (
              <Text key={line}>{line}</Text>
            ))}
            <HStack>
              <Button
                label={`Choose ${p.name}`}
                variant={p.id === 'byoa' ? 'secondary' : 'primary'}
                isLoading={busy === p.id}
                isDisabled={isDisabled || (!!busy && busy !== p.id)}
                onClick={() => void choose(p.id)}
              />
            </HStack>
          </VStack>
        </Card>
      ))}
    </>
  );
}
