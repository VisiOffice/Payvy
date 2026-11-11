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
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { Page } from '../Page';
import { billStatus, eventLabel } from '../status';
import { ReasonDialog } from '../inbox/ReasonDialog';
import { ReleaseDialog } from '../inbox/ReleaseDialog';
import { METHOD, type Bill as BillT } from '../inbox/types';
import { DueText } from '../Due';
import { date, money } from '../money';
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
  invoice_review?: { id:string; version:number; document_id:string; created_by:string; summary_reason:string | null; payload:{ rows: {id:string; description:string; amount:string; excluded_reason:string|null}[] } } | null;
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

      {detail.invoice_review && <Card><VStack gap={2}>
        <Text weight="semibold">Invoice review · revision {detail.invoice_review.version}</Text>
        <Text type="supporting">Recorded by {detail.invoice_review.created_by}. Review history and excluded rows remain with the source document.</Text>
        {detail.invoice_review.summary_reason && <Banner status="warning" title="Summary-only bill" description={detail.invoice_review.summary_reason} />}
        <Link href={`/documents/${detail.invoice_review.document_id}`}>View source and review history</Link>
      </VStack></Card>}
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
