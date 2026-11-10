import { useEffect, useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Dialog, DialogHeader } from '@astryxdesign/core/Dialog';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Table, type TableColumn } from '@astryxdesign/core/Table';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { useNavigate } from 'react-router';
import { Page } from '../Page';
import { run } from '../api/client';
import { money } from '../money';

/** A row of vendor.list. */
export interface VendorSummary extends Record<string, unknown> {
  id: string;
  name: string;
  open_bills: number;
  open_cents: number;
  own_workflow: boolean;
  workflow_proposals: number;
}

/** "Its own" or "The default", with any proposals waiting. */
export function WorkflowCell({ v }: { v: VendorSummary }) {
  return (
    <HStack gap={2} vAlign="center">
      {v.own_workflow ? <Text>Its own</Text> : <Text type="supporting">The default</Text>}
      {v.workflow_proposals > 0 && (
        <Badge
          variant="warning"
          label={`${v.workflow_proposals} proposed change${v.workflow_proposals === 1 ? '' : 's'}`}
        />
      )}
    </HStack>
  );
}

const columns: TableColumn<VendorSummary>[] = [
  {
    key: 'name',
    header: 'Vendor',
    renderCell: (row) => <Link href={`/vendors/${row.id}`}>{row.name}</Link>,
  },
  {
    key: 'open_bills',
    header: 'Open bills',
    align: 'end',
    renderCell: (row) => (row.open_bills ? String(row.open_bills) : '—'),
  },
  {
    key: 'open_cents',
    header: 'Open amount',
    align: 'end',
    renderCell: (row) => (row.open_bills ? money(row.open_cents) : '—'),
  },
  { key: 'own_workflow', header: 'Workflow', renderCell: (row) => <WorkflowCell v={row} /> },
];

/** Adds a vendor before its first bill, so its account numbers, bank
 *  details and workflow can be set up ahead of time. */
function AddVendorDialog({ existing, onClose }: { existing: VendorSummary[]; onClose: () => void }) {
  const navigate = useNavigate();
  const [name, setName] = useState('');
  const [email, setEmail] = useState('');
  const [terms, setTerms] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const same = existing.find((v) => v.name.trim().toLowerCase() === name.trim().toLowerCase());
  const termsDays = terms.trim() === '' ? undefined : Number(terms);
  const termsOk = termsDays === undefined || (Number.isInteger(termsDays) && termsDays >= 0 && termsDays <= 365);

  async function add() {
    setBusy(true);
    setError(null);
    try {
      const r = await run<{ id: string }>('vendor.create', {
        name: name.trim(),
        ...(email.trim() ? { email: email.trim() } : {}),
        ...(termsDays !== undefined ? { terms_days: termsDays } : {}),
      });
      navigate(`/vendors/${r.result.id}`);
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
      <DialogHeader title="Add vendor" onOpenChange={(o) => !o && close()} />
      <VStack gap={4} padding={4}>
        <Text type="supporting">
          Vendors are also added as their bills come in. Add one here to set up its account numbers, bank details or
          workflow before the first bill.
        </Text>
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
        {error && <Banner status="error" title={error} />}
        <HStack gap={2} hAlign="end">
          <Button label="Cancel" variant="ghost" isDisabled={busy} onClick={close} />
          <Button
            label="Add vendor"
            variant="primary"
            isDisabled={busy || !name.trim() || !!same || !termsOk}
            clickAction={add}
          />
        </HStack>
      </VStack>
    </Dialog>
  );
}

export function Vendors() {
  const [vendors, setVendors] = useState<VendorSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);

  useEffect(() => {
    run<{ vendors: VendorSummary[] }>('vendor.list', {})
      .then((r) => setVendors(r.result.vendors))
      .catch((e) => setError(e.message));
  }, []);

  return (
    <Page
      title="Vendors"
      description="Who you pay, what's open with each, and how their bills are approved."
      actions={vendors && <Button label="Add vendor" variant="primary" onClick={() => setAdding(true)} />}
    >
      {error && <Banner status="error" title="Couldn't load vendors" description={error} />}
      {!vendors && !error && <Spinner label="Loading vendors" />}
      {vendors && vendors.length === 0 && (
        <EmptyState title="No vendors yet" description="Vendors are added as their bills come in, or add one now to set it up ahead of time." />
      )}
      {vendors && vendors.length > 0 && <Table data={vendors} columns={columns} idKey="id" hasHover />}
      {adding && vendors && <AddVendorDialog existing={vendors} onClose={() => setAdding(false)} />}
    </Page>
  );
}
