import { useCallback, useEffect, useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Dialog, DialogHeader } from '@astryxdesign/core/Dialog';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { Selector } from '@astryxdesign/core/Selector';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Table, pixel, proportional, type TableColumn } from '@astryxdesign/core/Table';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { Page } from '../Page';
import { cents as toCents, dollars, money } from '../money';
import { ReasonDialog } from '../inbox/ReasonDialog';
import { useSession } from '../session';
import { useCan } from '../permissions';

export interface PoAmounts {
  authorized_cents: number;
  reserved_cents: number;
  consumed_cents: number;
  allowance_cents: number;
  available_cents: number;
  progress: string;
}

export interface OutsidePayment {
  id: string;
  po_id: string;
  vendor_id: string;
  amount_cents: number;
  method: 'card' | 'check' | 'ach' | 'wire' | 'cash' | 'other';
  paid_on: string;
  reference: string | null;
  note: string;
  invoice_number: string | null;
  declared_by: string;
  declared_by_source: boolean;
  declared_at: string;
  status: 'declared' | 'confirmed' | 'withdrawn';
  confirmed_by: string | null;
  confirmed_at: string | null;
  withdrawn_by: string | null;
  withdrawn_at: string | null;
  withdrawn_reason: string | null;
  bill_id: string | null;
}

export interface PurchaseOrder extends Record<string, unknown> {
  id: string;
  vendor_id: string;
  vendor_name: string;
  po_number: string;
  status: 'draft' | 'active' | 'suspended' | 'closed' | 'canceled';
  owner: string | null;
  activated_by: string | null;
  vendor_reference: string | null;
  service_address: string | null;
  period_start: string | null;
  period_end: string | null;
  final_period: boolean;
  metadata: Record<string, unknown>;
  amounts: PoAmounts;
  revision: { revision: number; authorized_cents: number; allowance_cents: number } | null;
  lines: { description: string; amount_cents: number }[];
  allocations: { id: string; bill_id: string | null; amount_cents: number; state: string; kind: string }[];
  outside_payments: OutsidePayment[];
}

const OUTSIDE_STATUS_BADGE: Record<OutsidePayment['status'], { label: string; variant: 'info' | 'success' | 'neutral' }> = {
  declared: { label: 'Declared', variant: 'info' },
  confirmed: { label: 'Confirmed', variant: 'success' },
  withdrawn: { label: 'Withdrawn', variant: 'neutral' },
};

const OUTSIDE_METHOD_LABEL: Record<OutsidePayment['method'], string> = {
  card: 'Card',
  check: 'Check',
  ach: 'ACH',
  wire: 'Wire',
  cash: 'Cash',
  other: 'Other',
};

const STATUS_BADGE: Record<PurchaseOrder['status'], { label: string; variant: 'success' | 'neutral' | 'warning' }> = {
  draft: { label: 'Draft', variant: 'neutral' },
  active: { label: 'Active', variant: 'success' },
  suspended: { label: 'Suspended', variant: 'warning' },
  closed: { label: 'Closed', variant: 'neutral' },
  canceled: { label: 'Canceled', variant: 'warning' },
};

const columns: TableColumn<PurchaseOrder>[] = [
  { key: 'po_number', header: 'PO', width: proportional(1), renderCell: (r) => <Text weight="semibold">{r.po_number}</Text> },
  { key: 'vendor_name', header: 'Vendor', width: proportional(2), renderCell: (r) => <Link href={`/vendors/${r.vendor_id}`}>{r.vendor_name}</Link> },
  {
    key: 'status', header: 'Status', width: proportional(2),
    renderCell: (r) => (
      <HStack gap={2} vAlign="center">
        <Badge label={STATUS_BADGE[r.status].label} variant={STATUS_BADGE[r.status].variant} />
        <Text type="supporting">{r.amounts.progress.replaceAll('_', ' ')}</Text>
      </HStack>
    ),
  },
  { key: 'authorized', header: 'Authorized', align: 'end', width: pixel(130), renderCell: (r) => money(r.amounts.authorized_cents) },
  { key: 'reserved', header: 'Reserved', align: 'end', width: pixel(130), renderCell: (r) => money(r.amounts.reserved_cents) },
  { key: 'consumed', header: 'Consumed', align: 'end', width: pixel(130), renderCell: (r) => money(r.amounts.consumed_cents) },
  {
    key: 'available', header: 'Available', align: 'end', width: pixel(130),
    // A closed or canceled PO takes no more bills; the server reports 0.
    renderCell: (r) => (r.status === 'closed' || r.status === 'canceled' ? <Text type="supporting">None</Text> : money(r.amounts.available_cents)),
  },
];

export function PurchaseOrders() {
  const { me } = useSession();
  const can = useCan();
  if (!me.features.purchase_orders) {
    return (
      <Page title="Purchase orders">
        <Banner
          status="info"
          title="Purchase orders are turned off"
          description={can('po.configure')
            ? 'Turn them on in Settings > Purchase orders.'
            : 'An admin can turn them on in Settings > Purchase orders.'}
        />
        {can('po.configure') && <Link href="/settings/purchase-orders">Purchase order settings</Link>}
      </Page>
    );
  }
  return <PurchaseOrderList />;
}

function PurchaseOrderList() {
  const { me } = useSession();
  const can = useCan();
  const [pos, setPos] = useState<PurchaseOrder[] | null>(null);
  const [selected, setSelected] = useState<PurchaseOrder | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [ending, setEnding] = useState<'po.close' | 'po.cancel' | 'po.suspend' | null>(null);
  const [declaring, setDeclaring] = useState(false);
  const [withdrawing, setWithdrawing] = useState<OutsidePayment | null>(null);
  const [revise, setRevise] = useState('');
  const toast = useToast();

  const load = useCallback(() => {
    run<{ purchase_orders: PurchaseOrder[] }>('po.list', {})
      .then((r) => {
        setPos(r.result.purchase_orders);
        setError(null);
        if (selected) {
          const fresh = r.result.purchase_orders.find((p) => p.id === selected.id);
          if (fresh) setSelected(fresh);
        }
      })
      .catch((e) => setError(e.message));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected?.id]);
  useEffect(load, [load]);

  async function act(name: 'po.activate' | 'po.reactivate', po: PurchaseOrder) {
    try {
      const r = await run<PurchaseOrder>(name, { po_id: po.id });
      setSelected(r.result);
      toast({ body: `${po.po_number} is active` });
      load();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  async function doRevise(po: PurchaseOrder) {
    const amount = toCents(revise);
    if (amount === null) return;
    try {
      const r = await run<PurchaseOrder>('po.update', { po_id: po.id, authorized_cents: amount, reason: 'revised in the app' });
      setSelected(r.result);
      setRevise('');
      toast({ body: `${po.po_number} revised` });
      load();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  const selectColumns: TableColumn<PurchaseOrder>[] = columns.map((c) =>
    c.key === 'po_number'
      ? { ...c, renderCell: (r: PurchaseOrder) => <Button label={r.po_number} variant="ghost" onClick={() => setSelected(r)} /> }
      : c,
  );

  return (
    <Page
      wide
      title="Purchase orders"
      description="What each vendor is authorized to bill, what's reserved and consumed against it, and what's left."
      actions={pos && <Button label="New purchase order" variant="primary" onClick={() => setCreating(true)} />}
    >
      {error && <Banner status="error" title="Couldn't load purchase orders" description={error} />}
      {!pos && !error && <Spinner label="Loading purchase orders" />}
      {pos && pos.length === 0 && (
        <EmptyState
          title="No purchase orders yet"
          description="Create one for a vendor, have a person activate it, and bills can be reserved against it as they arrive."
        />
      )}
      {pos && pos.length > 0 && (
        <div className="payvy-table payvy-grid"><Table columns={selectColumns} data={pos} idKey="id" hasHover /></div>
      )}

      {selected && (
        <VStack gap={3} padding={4}>
          <HStack gap={2} vAlign="center" wrap="wrap">
            <Text size="lg" weight="semibold">{selected.po_number}</Text>
            <Badge label={STATUS_BADGE[selected.status].label} variant={STATUS_BADGE[selected.status].variant} />
            <Text type="supporting">
              {selected.vendor_name}
              {selected.owner ? ` · owner ${selected.owner}` : ' · no owner'}
              {selected.revision ? ` · revision ${selected.revision.revision}` : ''}
            </Text>
          </HStack>
          <Text>
            Authorized {money(selected.amounts.authorized_cents)} · Reserved {money(selected.amounts.reserved_cents)} ·
            Consumed {money(selected.amounts.consumed_cents)} ·{' '}
            {selected.status === 'closed' || selected.status === 'canceled'
              ? `Nothing available (${STATUS_BADGE[selected.status].label.toLowerCase()})`
              : `Available ${money(selected.amounts.available_cents)}`}
            {selected.amounts.allowance_cents > 0 && ` (includes ${money(selected.amounts.allowance_cents)} overage allowance)`}
          </Text>
          {(selected.vendor_reference || selected.service_address || selected.period_start || selected.final_period) && (
            <Text type="supporting">
              {selected.final_period ? 'Final period' : 'Period'}
              {selected.period_start || selected.period_end
                ? ` ${selected.period_start ?? '…'} – ${selected.period_end ?? '…'}`
                : ''}
              {selected.vendor_reference ? ` · contract ${selected.vendor_reference}` : ''}
              {selected.service_address ? ` · ${selected.service_address}` : ''}
            </Text>
          )}
          {selected.lines.length > 0 && (
            <VStack gap={1}>
              {selected.lines.map((l, i) => (
                <HStack key={i} hAlign="between" gap={3}>
                  <Text>{l.description}</Text>
                  <Text hasTabularNumbers>{money(l.amount_cents)}</Text>
                </HStack>
              ))}
            </VStack>
          )}
          {selected.allocations.length > 0 && (
            <VStack gap={1}>
              <Text weight="semibold">Allocations</Text>
              {selected.allocations.map((a) => (
                <Text key={a.id} type="supporting">
                  {money(a.amount_cents)} · {a.state}
                  {a.kind === 'opening_consumption' ? ' · opening consumption from import' : ''}
                </Text>
              ))}
            </VStack>
          )}
          <HStack gap={2} wrap="wrap">
            {selected.status === 'draft' && (
              <Button label="Activate" variant="primary" clickAction={() => act('po.activate', selected)} />
            )}
            {selected.status === 'active' && (
              <Button label="Suspend…" variant="ghost" onClick={() => setEnding('po.suspend')} />
            )}
            {selected.status === 'suspended' && (
              <Button label="Reactivate" variant="primary" clickAction={() => act('po.reactivate', selected)} />
            )}
            {(selected.status === 'draft' || selected.status === 'active' || selected.status === 'suspended') && (
              <>
                <Button label="Close…" variant="ghost" onClick={() => setEnding('po.close')} />
                <Button label="Cancel…" variant="ghost" onClick={() => setEnding('po.cancel')} />
              </>
            )}
          </HStack>
          {selected.status !== 'closed' && selected.status !== 'canceled' && (
            <HStack gap={2} vAlign="end">
              <TextInput label="Revise authorized amount ($)" value={revise} onChange={setRevise} placeholder={dollars(selected.amounts.authorized_cents)} />
              <Button label="Revise" variant="secondary" onClick={() => doRevise(selected)} />
            </HStack>
          )}
          <VStack gap={2}>
            <HStack gap={2} hAlign="between" vAlign="center" wrap="wrap">
              <Text weight="semibold">Paid outside Payvy</Text>
              {(selected.status === 'active' || selected.status === 'suspended') && can('po.declare_paid') && (
                <Button
                  label="Record a payment made outside Payvy…"
                  variant="secondary"
                  size="sm"
                  onClick={() => setDeclaring(true)}
                />
              )}
            </HStack>
            {selected.outside_payments.length === 0 ? (
              <Text type="supporting">No outside payments declared on this PO.</Text>
            ) : (
              <VStack gap={2}>
                {selected.outside_payments.map((d) => (
                  <VStack key={d.id} gap={1} padding={3}>
                    <HStack gap={2} vAlign="center" hAlign="between" wrap="wrap">
                      <HStack gap={2} vAlign="center" wrap="wrap">
                        <Text weight="semibold" hasTabularNumbers>{money(d.amount_cents)}</Text>
                        <Text>{OUTSIDE_METHOD_LABEL[d.method] ?? d.method}</Text>
                        <Text type="supporting">{d.paid_on}</Text>
                        <Badge
                          label={d.bill_id ? 'Paid' : OUTSIDE_STATUS_BADGE[d.status].label}
                          variant={d.bill_id ? 'success' : OUTSIDE_STATUS_BADGE[d.status].variant}
                        />
                      </HStack>
                      <HStack gap={2}>
                        {d.status === 'declared' && can('po.confirm_paid') && d.declared_by !== me.person.actor && (
                          <Button
                            label="Confirm"
                            variant="primary"
                            size="sm"
                            clickAction={async () => {
                              try {
                                const r = await run<{ purchase_order: PurchaseOrder }>('po.confirm_paid', { declaration_id: d.id });
                                setSelected(r.result.purchase_order);
                                toast({ body: 'Outside payment confirmed' });
                                load();
                              } catch (e) {
                                toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
                              }
                            }}
                          />
                        )}
                        {d.status !== 'withdrawn' && !d.bill_id && can('po.withdraw_paid') && (
                          <Button
                            label="Withdraw…"
                            variant="ghost"
                            size="sm"
                            onClick={() => setWithdrawing(d)}
                          />
                        )}
                      </HStack>
                    </HStack>
                    <Text type="supporting">
                      Declared by {d.declared_by}
                      {d.reference ? ` · ref ${d.reference}` : ''}
                      {d.invoice_number ? ` · invoice ${d.invoice_number}` : ''}
                      {d.confirmed_by ? ` · confirmed by ${d.confirmed_by}` : ''}
                      {d.withdrawn_by ? ` · withdrawn by ${d.withdrawn_by} (${d.withdrawn_reason ?? ''})` : ''}
                    </Text>
                    <Text>{d.note}</Text>
                    {d.bill_id && (
                      <Text type="supporting">
                        Settled bill <Link href={`/bills/${d.bill_id}`}>{d.bill_id.slice(0, 8)}</Link>
                      </Text>
                    )}
                  </VStack>
                ))}
              </VStack>
            )}
          </VStack>
        </VStack>
      )}

      {creating && (
        <CreatePoDialog
          onClose={() => setCreating(false)}
          onSaved={(po) => {
            setCreating(false);
            setSelected(po);
            load();
          }}
        />
      )}
      {declaring && selected && (
        <DeclarePaidDialog
          po={selected}
          onClose={() => setDeclaring(false)}
          onSaved={(po) => {
            setDeclaring(false);
            setSelected(po);
            toast({ body: `Recorded outside payment on ${po.po_number}` });
            load();
          }}
        />
      )}
      {withdrawing && selected && (
        <ReasonDialog
          title={`Withdraw ${money(withdrawing.amount_cents)} declaration on ${selected.po_number}`}
          actionLabel="Withdraw"
          isRequired
          onClose={() => setWithdrawing(null)}
          onSubmit={async (reason) => {
            const r = await run<{ purchase_order: PurchaseOrder }>('po.withdraw_paid', {
              declaration_id: withdrawing.id,
              reason,
            });
            setSelected(r.result.purchase_order);
            setWithdrawing(null);
            toast({ body: 'Declaration withdrawn' });
            load();
          }}
        />
      )}
      {ending && selected && (
        <ReasonDialog
          title={`${ending === 'po.close' ? 'Close' : ending === 'po.suspend' ? 'Suspend' : 'Cancel'} ${selected.po_number}`}
          actionLabel={ending === 'po.close' ? 'Close' : ending === 'po.suspend' ? 'Suspend' : 'Cancel'}
          isRequired
          onClose={() => setEnding(null)}
          onSubmit={async (reason) => {
            const r = await run<PurchaseOrder>(ending, { po_id: selected.id, reason });
            setSelected(r.result);
            setEnding(null);
            load();
          }}
        />
      )}
    </Page>
  );
}

function CreatePoDialog({ onClose, onSaved }: { onClose: () => void; onSaved: (po: PurchaseOrder) => void }) {
  const [vendors, setVendors] = useState<{ id: string; name: string }[]>([]);
  const [vendorId, setVendorId] = useState('');
  const [number, setNumber] = useState('');
  const [owner, setOwner] = useState('');
  const [amount, setAmount] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    run<{ vendors: { id: string; name: string }[] }>('vendor.list', {})
      .then((r) => {
        setVendors(r.result.vendors);
        if (r.result.vendors[0]) setVendorId(r.result.vendors[0].id);
      })
      .catch(() => {});
  }, []);

  const cents = amount.trim() === '' ? null : toCents(amount);
  async function save() {
    if (!vendorId || !number.trim() || cents === null) return;
    setBusy(true);
    setError(null);
    try {
      const r = await run<PurchaseOrder>('po.create', {
        vendor_id: vendorId,
        po_number: number.trim(),
        ...(owner.trim() ? { owner: owner.trim() } : {}),
        authorized_cents: cents,
      });
      onSaved(r.result);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  return (
    <Dialog isOpen onOpenChange={(o) => !o && !busy && onClose()} width={480} purpose="form">
      <DialogHeader title="New purchase order" onOpenChange={(o) => !o && !busy && onClose()} />
      <VStack gap={4} padding={4}>
        <Text type="supporting">
          It starts as a draft. A person activates it before any bill can be reserved against it.
        </Text>
        <Selector
          label="Vendor"
          value={vendorId}
          onChange={setVendorId}
          options={vendors.map((v) => ({ value: v.id, label: v.name }))}
        />
        <TextInput label="PO number" value={number} onChange={setNumber} isRequired isDisabled={busy} />
        <TextInput label="Owner" value={owner} onChange={setOwner} placeholder="Who is responsible for this PO" isDisabled={busy} />
        <TextInput label="Authorized amount ($)" value={amount} onChange={setAmount} isRequired isDisabled={busy} />
        {error && <Banner status="error" title="Couldn't create it" description={error} />}
        <HStack gap={2}>
          <Button label="Create draft" variant="primary" onClick={save} isDisabled={busy || !vendorId || !number.trim() || cents === null} />
          <Button label="Cancel" variant="ghost" onClick={onClose} isDisabled={busy} />
        </HStack>
      </VStack>
    </Dialog>
  );
}

function DeclarePaidDialog({
  po,
  onClose,
  onSaved,
}: {
  po: PurchaseOrder;
  onClose: () => void;
  onSaved: (po: PurchaseOrder) => void;
}) {
  const [amount, setAmount] = useState('');
  const [method, setMethod] = useState<OutsidePayment['method']>('card');
  const [paidOn, setPaidOn] = useState(() => new Date().toISOString().slice(0, 10));
  const [reference, setReference] = useState('');
  const [invoiceNumber, setInvoiceNumber] = useState('');
  const [note, setNote] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const cents = amount.trim() === '' ? null : toCents(amount);

  async function save() {
    if (cents === null || !paidOn.trim() || !note.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const r = await run<{ purchase_order: PurchaseOrder }>('po.declare_paid', {
        po_id: po.id,
        amount_cents: cents,
        method,
        paid_on: paidOn.trim(),
        note: note.trim(),
        ...(reference.trim() ? { reference: reference.trim() } : {}),
        ...(invoiceNumber.trim() ? { invoice_number: invoiceNumber.trim() } : {}),
      });
      onSaved(r.result.purchase_order);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  return (
    <Dialog isOpen onOpenChange={(o) => !o && !busy && onClose()} width={480} purpose="form">
      <DialogHeader
        title={`Record payment made outside Payvy`}
        subtitle={`${po.po_number} · ${po.vendor_name}`}
        onOpenChange={(o) => !o && !busy && onClose()}
      />
      <VStack gap={4} padding={4}>
        <Text type="supporting">
          This blocks Payvy from paying matching bills on this PO. Someone else confirms it before the bill is marked paid.
        </Text>
        <TextInput label="Amount ($)" value={amount} onChange={setAmount} isRequired isDisabled={busy} />
        <Selector
          label="Method"
          value={method}
          onChange={(v) => setMethod(v as OutsidePayment['method'])}
          options={[
            { value: 'card', label: 'Card' },
            { value: 'check', label: 'Check' },
            { value: 'ach', label: 'ACH' },
            { value: 'wire', label: 'Wire' },
            { value: 'cash', label: 'Cash' },
            { value: 'other', label: 'Other' },
          ]}
        />
        <TextInput label="Paid on (YYYY-MM-DD)" value={paidOn} onChange={setPaidOn} isRequired isDisabled={busy} />
        <TextInput
          label="Reference (optional)"
          value={reference}
          onChange={setReference}
          placeholder="Check number, auth code, or bank trace (max 64 chars; never a card or account number)"
          isDisabled={busy}
        />
        <TextInput
          label="Invoice number (optional)"
          value={invoiceNumber}
          onChange={setInvoiceNumber}
          placeholder="Vendor invoice number, if known"
          isDisabled={busy}
        />
        <TextInput
          label="Note"
          value={note}
          onChange={setNote}
          placeholder="What it paid for and who agreed to it"
          isRequired
          isDisabled={busy}
        />
        {error && <Banner status="error" title="Couldn't record declaration" description={error} />}
        <HStack gap={2}>
          <Button
            label="Record payment"
            variant="primary"
            onClick={save}
            isDisabled={busy || cents === null || !paidOn.trim() || !note.trim()}
          />
          <Button label="Cancel" variant="ghost" onClick={onClose} isDisabled={busy} />
        </HStack>
      </VStack>
    </Dialog>
  );
}
