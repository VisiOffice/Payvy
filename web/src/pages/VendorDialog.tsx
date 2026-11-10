// Adding a vendor, or changing one's name, email and payment terms.
import { useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Dialog, DialogHeader } from '@astryxdesign/core/Dialog';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';

export interface VendorFields {
  id: string;
  name: string;
  email: string | null;
  terms_days: number | null;
}

/** Without `vendor` it adds one; with it, it edits that vendor. */
export function VendorDialog({ vendor, onClose, onSaved }: {
  vendor?: VendorFields;
  onClose: () => void;
  onSaved: (v: VendorFields) => void | Promise<void>;
}) {
  const [others, setOthers] = useState<{ id: string; name: string }[]>([]);
  const [name, setName] = useState(vendor?.name ?? '');
  const [email, setEmail] = useState(vendor?.email ?? '');
  const [terms, setTerms] = useState(vendor?.terms_days == null ? '' : String(vendor.terms_days));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    run<{ vendors: { id: string; name: string }[] }>('vendor.list', {})
      .then((r) => setOthers(r.result.vendors.filter((v) => v.id !== vendor?.id)))
      .catch(() => {});
  }, [vendor?.id]);

  const same = others.find((v) => v.name.trim().toLowerCase() === name.trim().toLowerCase());
  const termsDays = terms.trim() === '' ? null : Number(terms);
  const termsOk = termsDays === null || (Number.isInteger(termsDays) && termsDays >= 0 && termsDays <= 365);
  const emailChanged = !!vendor && (vendor.email ?? '') !== email.trim();

  async function save() {
    setBusy(true);
    setError(null);
    try {
      const fields = { name: name.trim(), email: email.trim() || null, terms_days: termsDays };
      const r = vendor
        ? await run<VendorFields>('vendor.update', { vendor_id: vendor.id, ...fields })
        : await run<VendorFields>('vendor.create', {
            name: fields.name,
            ...(fields.email ? { email: fields.email } : {}),
            ...(fields.terms_days !== null ? { terms_days: fields.terms_days } : {}),
          });
      await onSaved(r.result);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }
  const close = () => {
    if (!busy) onClose();
  };
  return (
    <Dialog isOpen onOpenChange={(o) => !o && close()} width={480} purpose="form">
      <DialogHeader title={vendor ? 'Edit vendor' : 'Add vendor'} onOpenChange={(o) => !o && close()} />
      <VStack gap={4} padding={4}>
        {!vendor && (
          <Text type="supporting">
            Vendors are also added as their bills come in. Add one here to set up its account numbers, bank details or
            workflow before the first bill.
          </Text>
        )}
        <TextInput label="Name" value={name} onChange={setName} isRequired isDisabled={busy} />
        <TextInput label="Email" type="email" value={email} onChange={setEmail} isDisabled={busy} />
        <TextInput
          label="Payment terms (days)"
          value={terms}
          onChange={setTerms}
          placeholder="e.g. 30 for net 30"
          isDisabled={busy}
        />
        {!termsOk && <Banner status="warning" title="Payment terms are 0 to 365 days" />}
        {same && (
          <Banner
            status="warning"
            title={`${same.name} is already a vendor`}
            description={<Link href={`/vendors/${same.id}`}>Open {same.name}</Link>}
          />
        )}
        {emailChanged && (
          <Text type="supporting">
            Invoices from this address&apos;s domain will match this vendor, and new payment-detail invitations go to
            it. Invitations already sent aren&apos;t changed.
          </Text>
        )}
        {error && <Banner status="error" title={error} />}
        <HStack gap={2} hAlign="end">
          <Button label="Cancel" variant="ghost" isDisabled={busy} onClick={close} />
          <Button
            label={vendor ? 'Save' : 'Add vendor'}
            variant="primary"
            isDisabled={busy || !name.trim() || !!same || !termsOk}
            clickAction={save}
          />
        </HStack>
      </VStack>
    </Dialog>
  );
}
