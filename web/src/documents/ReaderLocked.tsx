// Shown in place of every button that asks Payvy to read a document, for a
// hosted company whose plan has the reader off: a click there would only be
// refused by the server. The plan's name and price come from billing.get.
import { useEffect, useState } from 'react';
import { Link } from '@astryxdesign/core/Link';
import { Text } from '@astryxdesign/core/Text';
import { run } from '../api/client';
import { readerOffer, type Billing, type Offer } from '../billing';
import { useCan } from '../permissions';
import { useSession } from '../session';

/** True when Payvy's own reading isn't on this company's plan. Self-hosted
 *  installs have no plans, so it is never locked there. */
export function useReaderLocked(): boolean {
  const { plan } = useSession().me;
  return !!plan?.hosted && !plan.reader;
}

// One request per company for the whole visit: a review page shows this line
// in several places.
let cached: { org: string; offer: Promise<Offer | undefined> } | null = null;
function offerFor(org: string): Promise<Offer | undefined> {
  if (cached?.org !== org) {
    cached = {
      org,
      offer: run<Billing>('billing.get', {}).then((r) => readerOffer(r.result.offers), () => undefined),
    };
  }
  return cached.offer;
}

export function ReaderLocked() {
  const { me } = useSession();
  const can = useCan();
  const [offer, setOffer] = useState<Offer | undefined>();
  const billing = !!me.plan?.billing;
  useEffect(() => {
    if (!billing) return;
    let live = true;
    void offerFor(me.organization.id).then((o) => { if (live) setOffer(o); });
    return () => { live = false; };
  }, [billing, me.organization.id]);

  const line = offer ? `Included in ${offer.name} (${offer.price})` : "Payvy's own reading isn't on your plan";
  return (
    <span className="payvy-reader-locked">
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><rect x="3.5" y="7" width="9" height="6.5" rx="1.5" /><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" /></svg>
      {billing && can('billing.checkout')
        ? <Link href="/settings/billing">{line}</Link>
        : <Text type="supporting">{billing ? `${line}. Ask an admin to change the plan.` : line}</Text>}
    </span>
  );
}
