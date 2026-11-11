import { NotificationHistory } from '../notifications/History';
import { Allowed, useAccessVersion } from '../permissions';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useParams } from 'react-router';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { Heading } from '@astryxdesign/core/Heading';
import { Link } from '@astryxdesign/core/Link';
import { HStack } from '@astryxdesign/core/HStack';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { Page } from '../Page';
import { billStatus, eventLabel } from '../status';
import { ReasonDialog } from '../inbox/ReasonDialog';
import { ReleaseDialog } from '../inbox/ReleaseDialog';
import { METHOD, type Bill as BillT } from '../inbox/types';
import { DueText } from '../Due';
import { date, money, cents as toCents, dollars } from '../money';
import { DocumentViewer, type PageRef } from '../viewer/DocumentViewer';
import { highlightsOf, type DocumentDetail, type Read } from '../documents/types';
import { ReadFields } from '../documents/ReadFields';
import { waitingOnOthers, type ApprovalTurn } from '../approval';

interface Step {
  id: string;
  describe: string;
  state: 'done' | 'active' | 'pending' | 'skipped';
  at?: string;
  reason?: string;
}

interface Detail {
  bill: BillT & { created_by: string; created_at: string };
  lines: { description: string; amount_cents: number }[];
  progress: ({ status: string; waiting_on?: string[]; steps?: Step[] } & Record<string, unknown>) | string | null;
  approvals: { step_id: string; actor: string; at: string }[];
  /** While it waits for approval: whether you can give it, and who else can. */
  approval?: ApprovalTurn | null;
  can_release: boolean;
  payments: {
    id: string;
    state: string;
    method: string | null;
    amount_cents: number;
    created_at: string;
    external_ref?: string | null;
  }[];
  purchase_orders: {
    covered_cents: number;
    uncovered_cents: number;
    allocations: { allocation_id: string; po_id: string; po_number: string | null; amount_cents: number; state: string; kind: string }[];
    exception: { kind: string; excess_cents: number; detail: string } | null;
    overridden: boolean;
  };
  documents: { id: string; filename: string; content_type: string }[];
  /** The page arrangement of its document the bill pinned. */
  arrangement: { version: number; pages: PageRef[] | null } | null;
  history: { id: string; kind: string; actor: string; at: string; payload?: { summary?: string } }[];
}

const STEP_STATE: Record<Step['state'], { label: string; variant: 'success' | 'info' | 'neutral' }> = {
  done: { label: 'Done', variant: 'success' },
  active: { label: 'Now', variant: 'info' },
  pending: { label: 'Next', variant: 'neutral' },
  skipped: { label: 'Skipped', variant: 'neutral' },
};

export function BillPage() {
  const access = useAccessVersion();
  const { id } = useParams<{ id: string }>();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [read, setRead] = useState<Read | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [focus, setFocus] = useState<string | null>(null);
  const [open, setOpen] = useState<'release' | 'reject' | null>(null);
  const toast = useToast();

  const load = useCallback(() => {
    if (!id) return;
    run<Detail>('bill.get', { bill_id: id })
      .then((r) => {
        setDetail(r.result);
        setError(null);
        const doc = r.result.documents[0];
        if (doc) {
          run<DocumentDetail>('document.get', { document_id: doc.id })
            .then((d) => setRead(d.result.extractions[0]?.extraction.answer ?? null))
            .catch(() => setRead(null));
        }
      })
      .catch((e) => setError(e.message));
  }, [id]);
  useEffect(() => { setOpen(null); load(); }, [load, access]);

  const highlights = useMemo(() => highlightsOf(read), [read]);

  if (error)
    return (
      <Page title="Bill">
        <Banner status="error" title="Couldn't load this bill" description={error} />
      </Page>
    );
  if (!detail)
    return (
      <Page title="">
        <Spinner label="Loading the bill" />
      </Page>
    );

  const b = detail.bill;
  const p = detail.progress;
  const status = typeof p === 'string' ? p : (p?.status ?? b.state);
  const steps = typeof p === 'object' && p ? (p.steps ?? []) : [];
  const doc = detail.documents[0];

  async function approve() {
    try {
      await run('bill.approve', { bill_id: b.id });
      toast({ body: `Approved ${b.vendor_name} ${b.invoice_number}` });
      load();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  const isOpen = !['paid', 'released', 'rejected', 'void'].includes(status);
  const badge = billStatus(status);

  const side = (
    <VStack gap={5} padding={5}>
      <VStack gap={2}>
        <HStack gap={2} vAlign="center" wrap="wrap">
          <Heading level={1}>{b.vendor_name}</Heading>
          <Badge label={badge.label} variant={badge.variant} />
        </HStack>
        <Text type="supporting">
          Invoice {b.invoice_number} · <Link href={`/vendors/${b.vendor_id}`}>Vendor details</Link>
        </Text>
        <Text size="xl" weight="semibold" hasTabularNumbers>
          {money(b.amount_cents)}
        </Text>
      </VStack>

      {typeof p === 'object' && typeof p?.reason === 'string' && (
        <Banner status="info" title={p.reason} description="Its workflow pays automatically, but a person releases bills over the limit." />
      )}

      {isOpen && (
        <HStack gap={2}>
          {status === 'awaiting_approval' && detail.approval?.can_approve !== false && (
            <Allowed command="bill.approve"><Button label="Approve" variant="primary" clickAction={approve} /></Allowed>
          )}
          {status === 'awaiting_release' && detail.can_release && <Allowed command="bill.release"><Button label="Pay…" variant="primary" onClick={() => setOpen('release')} /></Allowed>}
          <Allowed command="bill.reject"><Button label="Reject" variant="ghost" onClick={() => setOpen('reject')} /></Allowed>
        </HStack>
      )}
      {status === 'awaiting_approval' && detail.approval && !detail.approval.can_approve && (
        <Banner status="info" title="Waiting on someone else" description={waitingOnOthers(detail.approval)} />
      )}

      <MetadataList>
        <MetadataListItem label="Invoice date">{date(b.invoice_date)}</MetadataListItem>
        <MetadataListItem label="Due">
          {b.due_date ? (
            <>
              {date(b.due_date)} {isOpen && <DueText iso={b.due_date} />}
            </>
          ) : (
            'Not set'
          )}
        </MetadataListItem>
        {b.memo && <MetadataListItem label="Memo">{b.memo}</MetadataListItem>}
        <MetadataListItem label="Entered by">{b.created_by}</MetadataListItem>
      </MetadataList>

      {read && <ReadFields read={read} focus={focus} onFocus={setFocus} />}

      {steps.length > 0 && (
        <VStack gap={2}>
          <Heading level={2}>Workflow</Heading>
          <ol className="payvy-steps">
            {steps.map((s) => (
              <li key={s.id}>
                <span className={`payvy-dot payvy-dot-${s.state}`} aria-hidden="true" />
                <Text>
                  {s.describe}{' '}
                  <Text type="supporting">
                    · {STEP_STATE[s.state].label}
                    {s.at ? ` · ${date(s.at)}` : ''}
                    {s.reason ? ` · ${s.reason}` : ''}
                  </Text>
                </Text>
              </li>
            ))}
          </ol>
        </VStack>
      )}

      {detail.lines.length > 0 && (
        <VStack gap={2}>
          <Heading level={2}>Lines</Heading>
          {detail.lines.map((l, i) => (
            <HStack key={i} hAlign="between" gap={3}>
              <Text>{l.description}</Text>
              <Text hasTabularNumbers>{money(l.amount_cents)}</Text>
            </HStack>
          ))}
        </VStack>
      )}

      <PoPanel billId={b.id} po={detail.purchase_orders} onChange={load} />

      {detail.payments.length > 0 && (
        <VStack gap={2}>
          <Heading level={2}>Payments</Heading>
          {detail.payments.map((pm) => (
            <Card key={pm.id}>
              <HStack hAlign="between" gap={3}>
                <VStack gap={0}>
                  <Text>{pm.method ? (METHOD[pm.method] ?? pm.method) : 'Paid elsewhere'}</Text>
                  <Text type="supporting">{date(pm.created_at)}</Text>
                </VStack>
                <VStack gap={0} hAlign="end">
                  <Text hasTabularNumbers>{money(pm.amount_cents)}</Text>
                  <Badge label={pm.state.replaceAll('_', ' ')} />
                </VStack>
              </HStack>
            </Card>
          ))}
        </VStack>
      )}

      <NotificationHistory billId={b.id} />

      {detail.history.length > 0 && (
        <VStack gap={2}>
          <Heading level={2}>History</Heading>
          <ol className="payvy-steps">
            {detail.history.map((h) => (
              <li key={h.id}>
                <span className="payvy-dot" aria-hidden="true" />
                <Text>
                  {eventLabel(h.kind)}
                  {h.kind === 'bill.pages_arranged' && h.payload?.summary ? `: ${h.payload.summary}` : ''}{' '}
                  <Text type="supporting">
                    {date(h.at)} · by {h.actor}
                  </Text>
                </Text>
              </li>
            ))}
          </ol>
        </VStack>
      )}
      {!doc && <Text type="supporting">No invoice is attached: this bill was entered without one.</Text>}
    </VStack>
  );

  return (
    <>
      {doc ? (
        <div className="payvy-split">
          <div className="payvy-split-side">{side}</div>
          <div className="payvy-split-doc">
            <DocumentViewer
              documentId={doc.id}
              contentType={doc.content_type}
              highlights={highlights}
              focus={focus}
              arrangement={detail.arrangement?.pages}
            />
          </div>
        </div>
      ) : (
        <div className="payvy-page payvy-page-narrow">{side}</div>
      )}

      {open === 'release' && (
        <ReleaseDialog
          bill={b}
          onClose={() => setOpen(null)}
          onReleased={(message) => {
            toast({ body: message });
            setOpen(null);
            load();
          }}
        />
      )}
      {open === 'reject' && (
        <ReasonDialog
          title={`Reject ${b.vendor_name} ${b.invoice_number}`}
          actionLabel="Reject"
          isRequired
          onClose={() => setOpen(null)}
          onSubmit={async (reason) => {
            await run('bill.reject', { bill_id: b.id, reason });
            toast({ body: 'Rejected' });
            setOpen(null);
            load();
          }}
        />
      )}
    </>
  );
}

interface PoPreview {
  candidates: { po_id: string; po_number: string; available_cents: number; proposed_cents: number; verified_reference: boolean }[];
}

/** PO matching beside the invoice: identify, reserve, accept, override.
 * Matching prepares the bill; it never approves or pays it. */
function PoPanel({ billId, po, onChange }: {
  billId: string;
  po: Detail['purchase_orders'];
  onChange: () => void;
}) {
  const [preview, setPreview] = useState<PoPreview | null>(null);
  const [amounts, setAmounts] = useState<Record<string, string>>({});
  const [excess, setExcess] = useState('');
  const [reason, setReason] = useState('');
  const [reverseId, setReverseId] = useState<string | null>(null);
  const toast = useToast();

  const loadPreview = useCallback(() => {
    run<{ result: PoPreview }>('po.match_preview', { bill_id: billId })
      .then((r) => {
        setPreview(r.result as unknown as PoPreview);
        const next: Record<string, string> = {};
        for (const c of (r.result as unknown as PoPreview).candidates) {
          if (c.proposed_cents > 0) next[c.po_id] = dollars(c.proposed_cents);
        }
        setAmounts(next);
      })
      .catch(() => setPreview(null));
  }, [billId]);
  useEffect(loadPreview, [loadPreview]);
  useEffect(() => {
    if (po.exception) setExcess(dollars(po.exception.excess_cents));
  }, [po.exception?.excess_cents]); // eslint-disable-line react-hooks/exhaustive-deps

  async function apply(poId: string) {
    const amount = toCents(amounts[poId] ?? '');
    if (amount === null) return;
    try {
      await run('po.match_apply', { bill_id: billId, allocations: [{ po_id: poId, amount_cents: amount }] });
      toast({ body: 'Reserved against the purchase order' });
      onChange();
      loadPreview();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  async function accept(allocationId: string) {
    try {
      await run('po.allocation_accept', { allocation_id: allocationId });
      onChange();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  async function override() {
    const amount = toCents(excess);
    if (amount === null || !reason.trim()) return;
    try {
      await run('po.override', { bill_id: billId, authorized_excess_cents: amount, reason: reason.trim() });
      toast({ body: 'Exception permitted for this bill' });
      setReason('');
      onChange();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  const candidates = preview?.candidates ?? [];
  return (
    <VStack gap={2}>
      <Heading level={2}>Purchase order</Heading>
      <Text>
        Covered {money(po.covered_cents)} · Uncovered {money(po.uncovered_cents)}
      </Text>
      {po.exception && !po.overridden && (
        <Banner
          status="warning"
          title={`Can't be paid yet: ${po.exception.kind.replaceAll('_', ' ')}`}
          description={po.exception.detail}
        />
      )}
      {po.overridden && <Badge label="Exception permitted by override" variant="warning" />}
      {po.allocations.map((a) => (
        <HStack key={a.allocation_id} hAlign="between" gap={3} vAlign="center">
          <Text>
            {a.po_number ?? 'PO'} · {money(a.amount_cents)} · {a.state}
          </Text>
          <HStack gap={2}>
            {a.state === 'reserved' && <Button label="Accept" variant="ghost" clickAction={() => accept(a.allocation_id)} />}
            <Button label="Reverse…" variant="ghost" onClick={() => setReverseId(a.allocation_id)} />
          </HStack>
        </HStack>
      ))}
      {candidates.map((c) => (
        <HStack key={c.po_id} gap={2} vAlign="end">
          <TextInput
            label={`${c.po_number} (${money(c.available_cents)} available)`}
            value={amounts[c.po_id] ?? ''}
            onChange={(v: string) => setAmounts((m) => ({ ...m, [c.po_id]: v }))}
          />
          <Button label="Reserve" variant="secondary" onClick={() => apply(c.po_id)} />
        </HStack>
      ))}
      {candidates.length === 0 && po.allocations.length === 0 && !po.exception && (
        <Text type="supporting">No active purchase order for this vendor.</Text>
      )}
      {po.exception && !po.overridden && (
        <VStack gap={2}>
          <TextInput label="Permit excess ($)" value={excess} onChange={setExcess} />
          <TextInput label="Reason (required)" value={reason} onChange={setReason} />
          <Button label="Permit this exception" variant="secondary" onClick={override} isDisabled={!reason.trim()} />
          <Text type="supporting">
            Permitting is bound to this bill, its POs and this excess. It does not add PO capacity.
          </Text>
        </VStack>
      )}
      {reverseId && (
        <ReasonDialog
          title="Reverse this allocation"
          actionLabel="Reverse"
          isRequired
          onClose={() => setReverseId(null)}
          onSubmit={async (r) => {
            await run('po.allocation_reverse', { allocation_id: reverseId, reason: r });
            setReverseId(null);
            onChange();
            loadPreview();
          }}
        />
      )}
    </VStack>
  );
}
