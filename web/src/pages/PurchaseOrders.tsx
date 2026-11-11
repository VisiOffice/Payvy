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
import { Table, type TableColumn } from '@astryxdesign/core/Table';
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

export interface PurchaseOrder extends Record<string, unknown> {
  id: string;
  vendor_id: string;
  vendor_name: string;
  po_number: string;
  status: 'draft' | 'active' | 'closed' | 'canceled';
  owner: string | null;
  activated_by: string | null;
  amounts: PoAmounts;
  revision: { revision: number; authorized_cents: number; allowance_cents: number } | null;
  lines: { description: string; amount_cents: number }[];
  allocations: { id: string; bill_id: string | null; amount_cents: number; state: string; kind: string }[];
}

const STATUS_BADGE: Record<PurchaseOrder['status'], { label: string; variant: 'success' | 'neutral' | 'warning' }> = {
  draft: { label: 'Draft', variant: 'neutral' },
  active: { label: 'Active', variant: 'success' },
  closed: { label: 'Closed', variant: 'neutral' },
  canceled: { label: 'Canceled', variant: 'warning' },
};

const columns: TableColumn<PurchaseOrder>[] = [
  { key: 'po_number', header: 'PO', renderCell: (r) => <Text weight="semibold">{r.po_number}</Text> },
  { key: 'vendor_name', header: 'Vendor', renderCell: (r) => <Link href={`/vendors/${r.vendor_id}`}>{r.vendor_name}</Link> },
  {
    key: 'status', header: 'Status',
    renderCell: (r) => (
      <HStack gap={2} vAlign="center">
        <Badge label={STATUS_BADGE[r.status].label} variant={STATUS_BADGE[r.status].variant} />
        <Text type="supporting">{r.amounts.progress.replaceAll('_', ' ')}</Text>
      </HStack>
    ),
  },
  { key: 'authorized', header: 'Authorized', align: 'end', renderCell: (r) => money(r.amounts.authorized_cents) },
  { key: 'reserved', header: 'Reserved', align: 'end', renderCell: (r) => money(r.amounts.reserved_cents) },
  { key: 'consumed', header: 'Consumed', align: 'end', renderCell: (r) => money(r.amounts.consumed_cents) },
  { key: 'available', header: 'Available', align: 'end', renderCell: (r) => money(r.amounts.available_cents) },
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
  const [pos, setPos] = useState<PurchaseOrder[] | null>(null);
  const [selected, setSelected] = useState<PurchaseOrder | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [ending, setEnding] = useState<'po.close' | 'po.cancel' | null>(null);
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

  async function act(name: 'po.activate', po: PurchaseOrder) {
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
        <Table columns={selectColumns} data={pos} idKey="id" hasHover />
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
            Consumed {money(selected.amounts.consumed_cents)} · Available {money(selected.amounts.available_cents)}
            {selected.amounts.allowance_cents > 0 && ` (includes ${money(selected.amounts.allowance_cents)} overage allowance)`}
          </Text>
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
            {(selected.status === 'draft' || selected.status === 'active') && (
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
      {ending && selected && (
        <ReasonDialog
          title={`${ending === 'po.close' ? 'Close' : 'Cancel'} ${selected.po_number}`}
          actionLabel={ending === 'po.close' ? 'Close' : 'Cancel'}
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
