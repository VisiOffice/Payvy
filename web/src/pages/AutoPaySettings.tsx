// Settings > Auto-pay limit: the most a workflow may pay without a person.
// A bill over it waits for a release, whatever its workflow says. Changing a
// limit is a person's act (both commands are person_only); lowering runs at
// once, raising shows the bills that would start paying and commits only
// with the preview's token, like a release.
import { useCallback, useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { Dialog, DialogHeader } from '@astryxdesign/core/Dialog';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { preview, run } from '../api/client';
import { cents, dollars, money } from '../money';
import { Page } from '../Page';

export interface Limits {
  company: { limit_cents: number; source: 'default' | 'company'; label: string };
  default_cents: number;
  max_cents: number;
  vendors: { vendor_id: string; vendor_name: string; limit_cents: number; in_force: boolean }[];
}

interface Listed {
  bill_id: string;
  vendor_name: string;
  invoice_number: string;
  amount_cents: number;
}

interface Changed extends Limits {
  change: {
    what: string;
    from_cents: number;
    to_cents: number;
    now_pays_automatically: Listed[];
    now_waits_for_a_person: Listed[];
  };
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

type Raise = { token: string; input: { vendor_id?: string; limit_cents: number | null }; change: Changed['change'] };

/**
 * Edits one limit: the company's (no vendorId) or a vendor's own. `current`
 * is the vendor's own limit, or null when it follows the company's.
 */
export function LimitEditor({
  vendorId,
  current,
  company,
  onSaved,
}: {
  vendorId?: string;
  current: number | null;
  company: number;
  onSaved: () => void;
}) {
  const toast = useToast();
  const [value, setValue] = useState(current == null ? '' : dollars(current));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [raise, setRaise] = useState<Raise | null>(null);
  useEffect(() => setValue(current == null ? '' : dollars(current)), [current]);

  const wanted = value.trim() === '' ? null : cents(value);
  const invalid = value.trim() !== '' && wanted === null;
  // What applies to this scope now: a vendor without its own follows the company.
  const now = vendorId ? Math.min(current ?? company, company) : company;

  async function save(limit: number | null) {
    setError(null);
    setBusy(true);
    const input = { ...(vendorId ? { vendor_id: vendorId } : {}), limit_cents: limit };
    try {
      const lower = limit !== null && limit < now;
      if (lower) {
        const r = await run<Changed>('auto_pay.lower_limit', { ...input, limit_cents: limit });
        const held = r.result.change.now_waits_for_a_person.length;
        toast({
          body: `Auto-pay limit is now ${money(limit)}${held ? `; ${held} bill${held === 1 ? '' : 's'} now wait for you` : ''}`,
        });
        onSaved();
      } else {
        const p = await preview<Changed>('auto_pay.raise_limit', input);
        setRaise({ token: p.confirm_token!, input, change: p.result.change });
      }
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }

  async function confirm() {
    if (!raise) return;
    setError(null);
    setBusy(true);
    try {
      await run<Changed>('auto_pay.raise_limit', raise.input, raise.token);
      toast({
        body:
          raise.input.limit_cents === null
            ? 'This vendor now follows the company limit'
            : `Auto-pay limit is now ${money(raise.input.limit_cents)}`,
      });
      setRaise(null);
      onSaved();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <VStack gap={3}>
      <HStack gap={2} wrap="wrap" vAlign="end">
        <TextInput
          label={vendorId ? 'Limit for this vendor' : 'Company limit'}
          value={value}
          onChange={setValue}
          placeholder={vendorId ? `Follows the company's ${money(company)}` : undefined}
          isDisabled={busy}
        />
        <Button
          label="Save limit"
          variant="primary"
          isDisabled={busy || invalid || wanted === null || wanted === (vendorId ? current : company)}
          clickAction={() => save(wanted)}
        />
        {vendorId && current !== null && (
          <Button label="Use company limit" variant="ghost" isDisabled={busy} clickAction={() => save(null)} />
        )}
      </HStack>
      {invalid && <Banner status="warning" title="Enter an amount in dollars, e.g. 5000 or 5,000.00" />}
      {error && <Banner status="error" title={error} />}
      {raise && (
        <Dialog isOpen onOpenChange={(o) => !o && setRaise(null)} width={480} purpose="form">
          <DialogHeader
            title={`Raise ${raise.change.what}?`}
            subtitle={`From ${money(raise.change.from_cents)} to ${money(raise.change.to_cents)}`}
            onOpenChange={(o) => !o && setRaise(null)}
          />
          <VStack gap={4} padding={4}>
            <Text>
              Bills up to {money(raise.change.to_cents)} will be paid without anyone releasing them, when their
              workflow pays automatically. A wrong amount read from an invoice, or a fake invoice, could be paid
              before anyone looks.
            </Text>
            {raise.change.now_pays_automatically.length > 0 ? (
              <VStack gap={2}>
                <Text weight="semibold">These bills will start paying automatically:</Text>
                <MetadataList>
                  {raise.change.now_pays_automatically.map((b) => (
                    <MetadataListItem key={b.bill_id} label={`${b.vendor_name} ${b.invoice_number}`}>
                      {money(b.amount_cents)}
                    </MetadataListItem>
                  ))}
                </MetadataList>
              </VStack>
            ) : (
              <Text type="supporting">No bill waiting now would start paying automatically.</Text>
            )}
            <HStack gap={2} hAlign="end">
              <Button label="Cancel" variant="ghost" onClick={() => setRaise(null)} />
              <Button label="Raise limit" variant="primary" isDisabled={busy} clickAction={confirm} />
            </HStack>
          </VStack>
        </Dialog>
      )}
    </VStack>
  );
}

export function AutoPaySettings() {
  const [limits, setLimits] = useState<Limits | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => {
    run<Limits>('auto_pay.limits', {})
      .then((r) => {
        setLimits(r.result);
        setError(null);
      })
      .catch((e) => setError(message(e)));
  }, []);
  useEffect(load, [load]);

  return (
    <Page
      title="Auto-pay limit"
      narrow
      description="The most a workflow may pay without a person. A bill over the limit waits in Waiting on you for someone to release it, whatever its workflow says."
    >
      {error && <Banner status="error" title="Couldn't load the auto-pay limit" description={error} />}
      {!limits && !error && <Spinner label="Loading" />}
      {limits && (
        <VStack gap={5}>
          <Card>
            <VStack gap={4}>
              <MetadataList>
                <MetadataListItem label="Limit now">
                  <Text weight="semibold" hasTabularNumbers>
                    {money(limits.company.limit_cents)}
                  </Text>{' '}
                  <Text type="supporting">
                    {limits.company.source === 'default' ? '(the default; no one has set it yet)' : ''}
                  </Text>
                </MetadataListItem>
              </MetadataList>
              <LimitEditor current={limits.company.limit_cents} company={limits.company.limit_cents} onSaved={load} />
              <Text type="supporting">
                Lowering takes effect at once. Raising shows which bills would start paying automatically, and asks
                you to confirm. Every change is in the history.
              </Text>
            </VStack>
          </Card>
          <VStack gap={3}>
            <Heading level={2}>Vendors with a lower limit</Heading>
            {limits.vendors.length === 0 ? (
              <Text type="supporting">
                None. Set one on a vendor's page to keep its automatic payments smaller than the company's.
              </Text>
            ) : (
              <Card>
                <MetadataList>
                  {limits.vendors.map((v) => (
                    <MetadataListItem key={v.vendor_id} label={v.vendor_name}>
                      <HStack gap={2} vAlign="center">
                        <Link href={`/vendors/${v.vendor_id}#auto-pay`}>{money(v.limit_cents)}</Link>
                        {!v.in_force && <Text type="supporting">(above the company's; the company's applies)</Text>}
                      </HStack>
                    </MetadataListItem>
                  ))}
                </MetadataList>
              </Card>
            )}
          </VStack>
        </VStack>
      )}
    </Page>
  );
}

/** The auto-pay limit section of a vendor's page. */
export function VendorAutoPayLimit({
  vendorId,
  limit,
  onSaved,
}: {
  vendorId: string;
  limit: { own_cents: number | null; in_force: { cents: number; source: string }; company_cents: number };
  onSaved: () => void;
}) {
  return (
    <section id="auto-pay" style={{ scrollMarginTop: 16 }}>
      <VStack gap={3}>
        <Heading level={2}>Auto-pay limit</Heading>
        <Card>
          <VStack gap={3}>
            <Text>
              Bills over {money(limit.in_force.cents)} wait for a person to release them, even if this vendor's
              workflow pays automatically.{' '}
              {limit.in_force.source === 'vendor' ? (
                <>This vendor has its own limit, below the company's {money(limit.company_cents)}.</>
              ) : (
                <>
                  That's the company's limit (<Link href="/settings/auto-pay">Settings</Link>); set a lower one for
                  this vendor here.
                </>
              )}
            </Text>
            <LimitEditor vendorId={vendorId} current={limit.own_cents ?? null} company={limit.company_cents} onSaved={onSaved} />
          </VStack>
        </Card>
      </VStack>
    </section>
  );
}
