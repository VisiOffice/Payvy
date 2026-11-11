import { Allowed, useCan } from '../permissions';
// One vendor: how its bills are approved and paid (edited here), its bills,
// where it's paid, and the account numbers it bills under.
import { useCallback, useEffect, useRef, useState } from 'react';
import { useLocation, useParams } from 'react-router';
import { Button } from '@astryxdesign/core/Button';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { BreadcrumbItem, Breadcrumbs } from '@astryxdesign/core/Breadcrumbs';
import { Card } from '@astryxdesign/core/Card';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { Page } from '../Page';
import { PayeeInvites, type PayeeInvite } from './PayeeInvites';
import { RampPayee } from './RampPayee';
import { VendorDialog } from './VendorDialog';
import { VendorAutoPayLimit } from './AutoPaySettings';
import { BillsTable, type Listed } from '../bills/BillsTable';
import { METHOD } from '../inbox/types';
import { date } from '../money';
import { describeStep, type Current } from '../workflow';
import { Builder, loadPeople } from '../workflows/Builder';
import type { PersonOption } from '../workflows/StepEditor';

interface VendorDetail {
  vendor: { id: string; name: string; email: string | null; terms_days: number | null };
  accounts: { id: string; account_number: string; location: string | null }[];
  workflow: Current;
  auto_pay_limit: Parameters<typeof VendorAutoPayLimit>[0]['limit'];
  payee_invites: PayeeInvite[];
  payees: {
    id: string;
    recipient_name: string;
    is_current_connection: boolean;
    method: string;
    destination: string;
    status: string;
    proposed_by: string;
    decided_by: string | null;
    created_at: string;
  }[];
}

export function VendorPage() {
  const can = useCan();
  const { id = '' } = useParams();
  const { hash } = useLocation();
  const [detail, setDetail] = useState<VendorDetail | null>(null);
  const [people, setPeople] = useState<PersonOption[] | null>(null);
  const [bills, setBills] = useState<Listed[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editingWorkflow, setEditingWorkflow] = useState(hash === '#workflow');
  const [editing, setEditing] = useState(false);
  const [accountNumber, setAccountNumber] = useState('');
  const [location, setLocation] = useState('');
  const [accountError, setAccountError] = useState<string | null>(null);
  const [accountBusy, setAccountBusy] = useState(false);
  const workflowRef = useRef<HTMLDivElement>(null);

  const loadVendor = useCallback(async () => {
    const r = await run<VendorDetail>('vendor.get', { vendor_id: id });
    setDetail(r.result);
  }, [id]);

  useEffect(() => {
    setDetail(null);
    setBills(null);
    Promise.all([
      loadVendor(),
      loadPeople().then(setPeople),
      run<{ bills: Listed[] }>('bill.list', { vendor_id: id, limit: 100 }).then((r) => setBills(r.result.bills)),
    ]).catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [id, loadVendor]);

  // A bank may have been replaced in another tab while this vendor stayed open.
  useEffect(() => {
    const refresh = () => { loadVendor().catch(e => setError(e instanceof Error ? e.message : String(e))); };
    window.addEventListener('focus', refresh);
    return () => window.removeEventListener('focus', refresh);
  }, [loadVendor]);

  // Links to a vendor's workflow (from Waiting on you, or Workflows) land on it.
  const ready = !!detail && !!people;
  useEffect(() => {
    if (ready && hash === '#workflow') { setEditingWorkflow(true); workflowRef.current?.scrollIntoView({ block: 'start' }); }
  }, [ready, hash]);

  if (error)
    return (
      <Page title="Vendor">
        <Banner status="error" title="Couldn't load this vendor" description={error} />
      </Page>
    );
  if (!detail || !people)
    return (
      <Page title="">
        <Spinner label="Loading" />
      </Page>
    );
  const { vendor } = detail;

  async function changeAccounts(change: () => Promise<unknown>) {
    setAccountBusy(true);
    setAccountError(null);
    try {
      await change();
      await loadVendor();
    } catch (e) {
      setAccountError(e instanceof Error ? e.message : String(e));
    } finally {
      setAccountBusy(false);
    }
  }
  const active = detail.payees.find((p) => p.status === 'active');
  const proposed = detail.payees.filter((p) => p.status === 'proposed');

  return (
    <Page
      title={vendor.name}
      actions={<Allowed command="vendor.update"><Button label="Edit vendor" variant="secondary" onClick={() => setEditing(true)} /></Allowed>}
      above={
        <Breadcrumbs>
          <BreadcrumbItem href="/vendors">Vendors</BreadcrumbItem>
          <BreadcrumbItem isCurrent>{vendor.name}</BreadcrumbItem>
        </Breadcrumbs>
      }
      description={
        <>
          {vendor.email ? `${vendor.email} · ` : ''}
          {vendor.terms_days == null
            ? 'No payment terms: a bill is due when its invoice says'
            : `Net ${vendor.terms_days}: a bill is due ${vendor.terms_days} days after its invoice date`}
        </>
      }
    >

      <VStack gap={3}>
        <Heading level={2}>Bills</Heading>
        {!bills && <Spinner label="Loading bills" />}
        {bills && bills.length === 0 && <EmptyState title="No bills from this vendor yet" />}
        {bills && bills.length > 0 && <BillsTable bills={bills} showVendor={false} />}
      </VStack>

      <HStack gap={6} wrap="wrap" vAlign="start">
        <VStack gap={3} width={420} maxWidth="100%">
          <Heading level={2}>Paid to</Heading>
          <Card>
            <VStack gap={3}>
              {active ? (
                <VStack gap={2}>
                {!active.is_current_connection && <Banner status="warning" title="This payee belongs to a previous bank connection" description="Choose and propose a payee for the current bank. A person must accept the new mapping before it can be used." />}
                <MetadataList>
                  <MetadataListItem label="Recipient">{active.recipient_name}</MetadataListItem>
                  <MetadataListItem label="How">
                    {METHOD[active.method] ?? active.method} · {active.destination}
                  </MetadataListItem>
                  <MetadataListItem label="Accepted by">{active.decided_by ?? '—'}</MetadataListItem>
                </MetadataList>
                </VStack>
              ) : (
                <Text type="supporting">
                  Not set. An AP clerk can propose a mapping; a payment releaser accepts it in Waiting on you.
                </Text>
              )}
              <RampPayee key={`ramp:${vendor.id}`} vendorId={vendor.id} vendorName={vendor.name} onChanged={loadVendor} />
              <PayeeInvites key={vendor.id} vendor={vendor} invites={detail.payee_invites} payees={detail.payees} onChanged={loadVendor} />
              {proposed.map((p) => (
                <HStack key={p.id} gap={2} vAlign="center" wrap="wrap">
                  <Badge variant="warning" label={p.is_current_connection ? "Change proposed" : "Previous connection"} />
                  <Text>
                    {p.recipient_name} · {METHOD[p.method] ?? p.method} · {p.destination}, by {p.proposed_by} on{' '}
                    {date(p.created_at)}.
                  </Text>
                  <Link href="/">Review in Waiting on you</Link>
                </HStack>
              ))}
            </VStack>
          </Card>
        </VStack>

        <VStack gap={3} width={420} maxWidth="100%">
          <Heading level={2}>Account numbers</Heading>
          <Card>
            <VStack gap={3}>
              {detail.accounts.length === 0 ? (
                <Text type="supporting">None yet. Invoices that print one help match bills to this vendor.</Text>
              ) : (
                <MetadataList>
                  {detail.accounts.map((a) => (
                    <MetadataListItem key={a.id} label={a.account_number}>
                      <HStack gap={2} vAlign="center" hAlign="between">
                        <Text>{a.location ?? '—'}</Text>
                        <Allowed command="vendor.remove_account"><Button
                          label="Remove"
                          aria-label={`Remove account number ${a.account_number}`}
                          variant="ghost"
                          size="sm"
                          isDisabled={accountBusy}
                          clickAction={() =>
                            changeAccounts(() => run('vendor.remove_account', { vendor_id: vendor.id, account_id: a.id }))
                          }
                        /></Allowed>
                      </HStack>
                    </MetadataListItem>
                  ))}
                </MetadataList>
              )}
              <HStack gap={2} wrap="wrap" vAlign="end">
                <TextInput label="Account number" value={accountNumber} onChange={setAccountNumber} isDisabled={accountBusy} />
                <TextInput label="Location" value={location} onChange={setLocation} placeholder="Optional, e.g. a job site" isDisabled={accountBusy} />
                <Allowed command="vendor.add_account"><Button
                  label="Add"
                  variant="secondary"
                  isDisabled={accountBusy || !accountNumber.trim()}
                  clickAction={() =>
                    changeAccounts(async () => {
                      await run('vendor.add_account', {
                        vendor_id: vendor.id,
                        account_number: accountNumber.trim(),
                        ...(location.trim() ? { location: location.trim() } : {}),
                      });
                      setAccountNumber('');
                      setLocation('');
                    })
                  }
                /></Allowed>
              </HStack>
              {accountError && <Banner status="error" title={accountError} />}
            </VStack>
          </Card>
        </VStack>
      </HStack>
      <VendorAutoPayLimit vendorId={vendor.id} limit={detail.auto_pay_limit} onSaved={() => void loadVendor()} />
      <section ref={workflowRef} id="workflow" className="payvy-vendor-workflow" style={{ scrollMarginTop: 16 }}>
        <div className="payvy-section-heading">
          <Heading level={2}>Workflow</Heading>
          <span className="payvy-muted">{detail.workflow.source === 'vendor' ? 'For this vendor' : 'Company default'}</span>
          <Allowed command="workflow.propose"><Button label={editingWorkflow ? 'Close editor' : 'Edit workflow'} variant="secondary" size="sm"
            onClick={() => setEditingWorkflow(v => !v)} aria-expanded={editingWorkflow} /></Allowed>
        </div>
        {editingWorkflow ? <Builder vendor={{ id: vendor.id, name: vendor.name }} current={detail.workflow}
          people={people} onSaved={loadVendor} /> : <div className="payvy-workflow-summary">
          {detail.workflow.workflow.steps.map((step, index) => <div key={step.id}>
            <span className="payvy-step-number">{index + 1}</span>
            <span>{describeStep(step, actor => people.find(p => p.value === actor)?.label ?? actor)}</span>
          </div>)}
          {can('workflow.accept') && detail.workflow.proposals.length > 0 && <Button label={`Review ${detail.workflow.proposals.length} proposed changes`}
            variant="secondary" onClick={() => setEditingWorkflow(true)} />}
        </div>}
      </section>
      {editing && (
        <VendorDialog
          vendor={vendor}
          onClose={() => setEditing(false)}
          onSaved={async () => {
            await loadVendor();
            setEditing(false);
          }}
        />
      )}
    </Page>
  );
}
