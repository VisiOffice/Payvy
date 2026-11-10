import { useCallback, useEffect, useMemo, useState } from 'react';
import { useParams } from 'react-router';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { ReasonDialog } from '../inbox/ReasonDialog';
import { ReleaseDialog } from '../inbox/ReleaseDialog';
import { METHOD, type Bill as BillT } from '../inbox/types';
import { date, money } from '../money';
import { DocumentViewer } from '../viewer/DocumentViewer';
import { highlightsOf, type DocumentDetail, type Read } from '../documents/types';
import { ReadFields } from '../documents/ReadFields';

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
  payments: {
    id: string;
    state: string;
    method: string | null;
    amount_cents: number;
    created_at: string;
    external_ref?: string | null;
  }[];
  documents: { id: string; filename: string; content_type: string }[];
  history: { id: string; kind: string; actor: string; at: string }[];
}

const STATE_VARIANT: Record<Step['state'], 'success' | 'warning' | 'neutral'> = {
  done: 'success',
  active: 'warning',
  pending: 'neutral',
  skipped: 'neutral',
};

export function BillPage() {
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
  useEffect(load, [load]);

  const highlights = useMemo(() => highlightsOf(read), [read]);

  if (error) return <Banner status="error" title="Couldn't load this bill" description={error} />;
  if (!detail) return <Spinner label="Loading the bill" />;

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

  return (
    <div style={{ display: 'flex', height: '100%', minHeight: 0 }}>
      <div style={{ flex: '0 0 440px', overflow: 'auto', borderRight: '1px solid var(--color-border)' }}>
        <VStack gap={5} padding={5}>
          <VStack gap={2}>
            <HStack gap={2} vAlign="center" wrap="wrap">
              <Heading level={1}>{b.vendor_name}</Heading>
              <Badge label={status.replaceAll('_', ' ')} variant={status === 'paid' ? 'success' : 'warning'} />
            </HStack>
            <Text type="supporting">Invoice {b.invoice_number}</Text>
            <Text size="xl" weight="semibold" hasTabularNumbers>
              {money(b.amount_cents)}
            </Text>
          </VStack>

          {isOpen && (
            <HStack gap={2}>
              {status === 'awaiting_approval' && <Button label="Approve" variant="primary" clickAction={approve} />}
              {status === 'awaiting_release' && <Button label="Pay…" variant="primary" onClick={() => setOpen('release')} />}
              <Button label="Reject" variant="ghost" onClick={() => setOpen('reject')} />
            </HStack>
          )}

          <MetadataList>
            <MetadataListItem label="Invoice date">{date(b.invoice_date)}</MetadataListItem>
            <MetadataListItem label="Due">{b.due_date ? date(b.due_date) : 'Not set'}</MetadataListItem>
            {b.memo && <MetadataListItem label="Memo">{b.memo}</MetadataListItem>}
            <MetadataListItem label="Entered by">{b.created_by}</MetadataListItem>
          </MetadataList>

          {read && <ReadFields read={read} focus={focus} onFocus={setFocus} />}

          {steps.length > 0 && (
            <VStack gap={2}>
              <Heading level={2}>Workflow</Heading>
              {steps.map((s) => (
                <HStack key={s.id} gap={2} vAlign="center">
                  <Badge label={s.state} variant={STATE_VARIANT[s.state]} />
                  <Text>{s.describe}</Text>
                  {s.at && <Text type="supporting">{date(s.at)}</Text>}
                  {s.reason && <Text type="supporting">{s.reason}</Text>}
                </HStack>
              ))}
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

          {detail.history.length > 0 && (
            <VStack gap={2}>
              <Heading level={2}>History</Heading>
              {detail.history.map((h) => (
                <HStack key={h.id} gap={2}>
                  <Text type="supporting">{date(h.at)}</Text>
                  <Text>{h.kind}</Text>
                  <Text type="supporting">by {h.actor}</Text>
                </HStack>
              ))}
            </VStack>
          )}
        </VStack>
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        {doc ? (
          <DocumentViewer documentId={doc.id} contentType={doc.content_type} highlights={highlights} focus={focus} />
        ) : (
          <EmptyState title="No document" description="This bill was entered without an invoice attached." />
        )}
      </div>

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
    </div>
  );
}
