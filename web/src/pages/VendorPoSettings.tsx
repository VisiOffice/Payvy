// A vendor's purchase-order rules, on its own page: whether its bills
// require a PO, whether they wait for the customer to fund them, and how
// far a bill may run over a PO (overage caps). Only shown when purchase
// orders are on for the company; only an admin changes them
// (po.set_vendor_settings). Caps are pinned onto each PO revision when it
// is made, so changing them here never rewrites an existing PO.
import { useCallback, useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Switch } from '@astryxdesign/core/Switch';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { Allowed, useCan } from '../permissions';
import { run } from '../api/client';
import { cents as toCents, dollars } from '../money';

interface VendorPo {
  vendor_id: string;
  require_po: boolean;
  require_funding: boolean;
  overage_cents_cap: number | null;
  overage_percent_bp: number | null;
}

export function VendorPoSettings({ vendorId }: { vendorId: string }) {
  const [on, setOn] = useState<boolean | null>(null);
  const [data, setData] = useState<VendorPo | null>(null);
  const [capDollars, setCapDollars] = useState('');
  const [capPercent, setCapPercent] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const toast = useToast();
  const canSet = useCan()('po.set_vendor_settings');

  const load = useCallback(async () => {
    try {
      const settings = (await run<{ enabled: boolean }>('po.settings', {})).result;
      setOn(settings.enabled);
      if (!settings.enabled) return;
      const v = (await run<VendorPo>('po.vendor_settings', { vendor_id: vendorId })).result;
      setData(v);
      setCapDollars(v.overage_cents_cap == null ? '' : dollars(v.overage_cents_cap));
      setCapPercent(v.overage_percent_bp == null ? '' : String(v.overage_percent_bp / 100));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [vendorId]);
  useEffect(() => { void load(); }, [load]);

  // Purchase orders off for the company: this vendor has no PO rules to show.
  if (on === false) return null;
  if (error)
    return (
      <section>
        <VStack gap={3}>
          <Heading level={2}>Purchase orders</Heading>
          <Banner status="error" title="Couldn't load this vendor's PO settings" description={error} />
        </VStack>
      </section>
    );
  if (!data) return null;

  const capCents = capDollars.trim() === '' ? null : toCents(capDollars);
  const pct = capPercent.trim() === '' ? null : Number(capPercent);
  const capBp = pct === null ? null : Math.round(pct * 100);
  const capsOk =
    (capDollars.trim() === '' || capCents !== null) &&
    (capPercent.trim() === '' || (pct !== null && Number.isFinite(pct) && pct >= 0 && pct <= 100));

  async function save(patch: Partial<VendorPo>) {
    if (!data) return;
    setBusy(true);
    try {
      const next = { ...data, ...patch };
      const r = await run<VendorPo>('po.set_vendor_settings', {
        vendor_id: vendorId,
        require_po: next.require_po,
        require_funding: next.require_funding,
        overage_cents_cap: next.overage_cents_cap,
        overage_percent_bp: next.overage_percent_bp,
      });
      setData(r.result);
      toast({ body: 'PO settings saved' });
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
      await load();
    } finally {
      setBusy(false);
    }
  }

  return (
    <section id="purchase-orders" style={{ scrollMarginTop: 16 }}>
      <VStack gap={3}>
        <Heading level={2}>Purchase orders</Heading>
        <Card>
          <VStack gap={3}>
            <Switch
              label="Bills from this vendor require a PO"
              value={data.require_po}
              isDisabled={busy || !canSet}
              changeAction={(v: boolean) => void save({ require_po: v })}
              description="Bills that aren't matched to a PO, or aren't fully covered by one, can't be paid until they're matched or a person permits the exception."
            />
            <Switch
              label="Wait for the customer to fund the bill"
              value={data.require_funding}
              isDisabled={busy || !canSet}
              changeAction={(v: boolean) => void save({ require_funding: v })}
              description="Payment waits for a customer_funded signal on the bill. The signal is a report, not verified payment."
            />
            <Text weight="semibold">How far a bill may run over a PO</Text>
            <HStack gap={2} wrap="wrap" vAlign="end">
              <TextInput
                label="Overage cap ($)"
                value={capDollars}
                onChange={setCapDollars}
                placeholder="Blank = no cap set"
                isDisabled={busy || !canSet}
              />
              <TextInput
                label="Overage cap (%)"
                value={capPercent}
                onChange={setCapPercent}
                placeholder="Blank = no cap set"
                isDisabled={busy || !canSet}
              />
              <Allowed command="po.set_vendor_settings">
                <Button
                  label="Save caps"
                  variant="secondary"
                  isDisabled={busy || !capsOk}
                  clickAction={() => void save({ overage_cents_cap: capCents, overage_percent_bp: capBp })}
                />
              </Allowed>
            </HStack>
            {!capsOk && <Banner status="warning" title="Caps are a dollar amount and a percentage from 0 to 100" />}
            <Text type="supporting">
              Zero is the default: no overage at all. If both caps are set, the smaller one governs — a
              $1,000 PO with a $25 cap and a 5% cap may run to $1,025 in total, across every bill, not per
              bill. Blank means not configured, which is also zero. The caps are pinned onto each PO when
              it is made or revised, so changing them here applies to new and revised POs, never rewrites
              one that already exists.
            </Text>
            {!canSet && <Text type="supporting">Only an admin changes these settings.</Text>}
          </VStack>
        </Card>
      </VStack>
    </section>
  );
}
