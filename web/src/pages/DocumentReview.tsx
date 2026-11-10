import { useCallback, useEffect, useMemo, useState, type ComponentProps, type ReactNode } from 'react';
import { useNavigate, useParams } from 'react-router';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { DateInput } from '@astryxdesign/core/DateInput';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { Selector } from '@astryxdesign/core/Selector';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { ApiError, run } from '../api/client';
import { ReasonDialog } from '../inbox/ReasonDialog';
import { cents, date, dollars, money } from '../money';
import { DocumentViewer } from '../viewer/DocumentViewer';
import { highlightsOf, type BillLine, type DocumentDetail, type ExtractionSummary } from '../documents/types';

interface Vendor {
  id: string;
  name: string;
  email: string | null;
}

interface Form {
  vendor_id: string;
  invoice_number: string;
  invoice_date: string;
  due_date: string;
  /** Dollars as typed, e.g. "1,234.56". */
  amount: string;
  memo: string;
}

const EMPTY: Form = { vendor_id: '', invoice_number: '', invoice_date: '', due_date: '', amount: '', memo: '' };


type ISODate = NonNullable<ComponentProps<typeof DateInput>['value']>;
/** A form date for DateInput: YYYY-MM-DD, or nothing. */
const iso = (s: string): ISODate | undefined => (/^\d{4}-\d{2}-\d{2}$/.test(s) ? (s as ISODate) : undefined);

/** The form as the model suggested it: its bill input if it made one, else its draft. */
function suggested(x: ExtractionSummary | undefined): Form {
  if (!x) return EMPTY;
  const b = x.bill_input;
  if (b) {
    return {
      vendor_id: b.vendor_id,
      invoice_number: b.invoice_number,
      invoice_date: b.invoice_date,
      due_date: b.due_date ?? '',
      amount: dollars(b.amount_cents),
      memo: b.memo ?? '',
    };
  }
  const d = x.extraction.draft;
  const m = x.extraction.vendor_match;
  return {
    vendor_id: m?.result === 'matched' ? m.vendor_id : '',
    invoice_number: d?.invoice_number ?? '',
    invoice_date: d?.invoice_date ?? '',
    due_date: d?.due_date ?? '',
    amount: d?.total_cents ? dollars(d.total_cents) : '',
    memo: d?.po_number ? `PO ${d.po_number}` : '',
  };
}

/**
 * One inbox document beside what was read from it: correct anything that's
 * wrong, then make it a bill. Corrections are recorded against the reading.
 */
export function DocumentReview() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const toast = useToast();
  const [detail, setDetail] = useState<DocumentDetail | null>(null);
  const [vendors, setVendors] = useState<Vendor[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<Form>(EMPTY);
  const [focus, setFocus] = useState<string | null>(null);
  const [duplicate, setDuplicate] = useState<string | null>(null);
  const [trashing, setTrashing] = useState(false);

  const load = useCallback(async () => {
    if (!id) return;
    try {
      const [d, v] = await Promise.all([
        run<DocumentDetail>('document.get', { document_id: id }),
        run<{ vendors: Vendor[] }>('vendor.list', {}),
      ]);
      setDetail(d.result);
      setVendors(v.result.vendors);
      setForm(suggested(d.result.extractions[0]));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [id]);
  useEffect(() => {
    void load();
  }, [load]);

  const latest = detail?.extractions[0];
  const highlights = useMemo(() => highlightsOf(latest?.extraction.answer), [latest]);

  if (error) return <Banner status="error" title="Couldn't load this document" description={error} />;
  if (!detail) return <Spinner label="Loading the document" />;

  const doc = detail.document;
  const x = latest?.extraction;
  const amountCents = cents(form.amount);
  // The suggested lines only add up while the amount is the one they were read with.
  const lines: BillLine[] | undefined =
    latest?.bill_input?.lines && amountCents === latest.bill_input.amount_cents ? latest.bill_input.lines : undefined;
  const canCreate =
    doc.status === 'inbox' && !!form.vendor_id && !!form.invoice_number.trim() && !!form.invoice_date && !!amountCents;
  const set = (patch: Partial<Form>) => setForm((f) => ({ ...f, ...patch }));

  async function readAgain() {
    try {
      await run('document.extract', { document_id: doc.id, again: true });
      await load();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  async function addVendor() {
    const nv = latest?.new_vendor;
    if (!nv) return;
    try {
      const r = await run<Vendor>('vendor.create', { name: nv.name, ...(nv.email ? { email: nv.email } : {}) });
      setVendors((vs) => [...vs, r.result].sort((a, b) => a.name.localeCompare(b.name)));
      set({ vendor_id: r.result.id });
      toast({ body: `Added ${r.result.name}` });
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  async function create(allowDuplicate = false) {
    if (!canCreate || !amountCents) return;
    try {
      const r = await run<{ bill: { id: string } }>('bill.create', {
        vendor_id: form.vendor_id,
        invoice_number: form.invoice_number.trim(),
        invoice_date: form.invoice_date,
        amount_cents: amountCents,
        document_id: doc.id,
        ...(form.due_date ? { due_date: form.due_date } : {}),
        ...(form.memo.trim() ? { memo: form.memo.trim() } : {}),
        ...(lines ? { lines } : {}),
        ...(x?.status === 'read' ? { extraction_id: x.id } : {}),
        ...(allowDuplicate ? { allow_duplicate: true } : {}),
      });
      toast({ body: 'Bill created' });
      navigate(`/bills/${r.result.bill.id}`);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      if (e instanceof ApiError && e.code === 'conflict' && message.includes('allow_duplicate')) {
        setDuplicate(message);
      } else {
        toast({ body: message, type: 'error' });
      }
    }
  }

  const match = x?.vendor_match;
  const vendorNote =
    match?.result === 'matched'
      ? `Matched ${match.name} (${match.confidence}): ${match.reasons.join('; ')}`
      : match?.result === 'ambiguous'
        ? `More than one vendor fits: ${match.vendor_ids
            .map((v) => vendors.find((x) => x.id === v)?.name ?? v)
            .join(', ')}. Choose one.`
        : undefined;

  return (
    <div className="payvy-split">
      <div className="payvy-split-side">
        <VStack gap={5} padding={5}>
          <VStack gap={1}>
            <Heading level={1}>{doc.filename}</Heading>
            <Text type="supporting">
              {doc.source === 'email' ? 'Emailed' : 'Uploaded'} {date(doc.received_at)}
              {x ? ` · read by ${x.model}` : ''}
            </Text>
          </VStack>

          {doc.status === 'filed' && doc.bill_id && (
            <Banner
              status="success"
              title="This document is a bill"
              description={<Link href={`/bills/${doc.bill_id}`}>Open the bill</Link>}
            />
          )}
          {doc.status === 'trashed' && (
            <Banner status="info" title="Set aside: not a bill" description={doc.trashed_reason ?? undefined} />
          )}

          {!x && doc.status === 'inbox' && (
            <Banner
              status="info"
              title="Not read yet"
              description="Read it with the model, or fill in the bill yourself."
              endContent={<Button label="Read it" size="sm" clickAction={readAgain} />}
            />
          )}
          {x?.status === 'failed' && (
            <Banner
              status="error"
              title="Reading it failed"
              description={x.error ?? undefined}
              endContent={<Button label="Try again" size="sm" clickAction={readAgain} />}
            />
          )}
          {latest && latest.failed_checks.length > 0 && (
            <Banner
              status="warning"
              title="Check these before making it a bill"
              description={(x?.checks ?? [])
                .filter((c) => !c.ok)
                .map((c) => c.detail)
                .join(' ')}
            />
          )}

          {doc.status === 'inbox' && (
            <VStack gap={4}>
              <Field focusKey="vendor_name" onFocus={setFocus}>
                <Selector
                  label="Vendor"
                  isRequired
                  value={form.vendor_id || undefined}
                  onChange={(v) => set({ vendor_id: v })}
                  placeholder="Choose a vendor"
                  description={vendorNote}
                  options={vendors.map((v) => ({ value: v.id, label: v.name }))}
                />
              </Field>
              {latest?.new_vendor && !form.vendor_id && (
                <HStack gap={2} vAlign="center">
                  <Text type="supporting">No vendor on file matches.</Text>
                  <Button label={`Add ${latest.new_vendor.name}`} size="sm" clickAction={addVendor} />
                </HStack>
              )}
              <Field focusKey="invoice_number" onFocus={setFocus}>
                <TextInput
                  label="Invoice number"
                  isRequired
                  value={form.invoice_number}
                  onChange={(v) => set({ invoice_number: v })}
                />
              </Field>
              <HStack gap={3} wrap="wrap">
                <Field focusKey="invoice_date" onFocus={setFocus}>
                  <DateInput
                    label="Invoice date"
                    isRequired
                    format="date"
                    value={iso(form.invoice_date)}
                    onChange={(v) => set({ invoice_date: v ?? '' })}
                  />
                </Field>
                <Field focusKey="due_date" onFocus={setFocus}>
                  <DateInput
                    label="Due"
                    format="date"
                    description="Blank: the vendor's terms"
                    value={iso(form.due_date)}
                    onChange={(v) => set({ due_date: v ?? '' })}
                  />
                </Field>
              </HStack>
              <Field focusKey="total" onFocus={setFocus}>
                <TextInput
                  label="Amount"
                  isRequired
                  value={form.amount}
                  onChange={(v) => set({ amount: v })}
                  placeholder="0.00"
                  status={form.amount && amountCents === null ? { type: 'error', message: 'Like 1,234.56' } : undefined}
                />
              </Field>
              <Field focusKey="po_number" onFocus={setFocus}>
                <TextInput label="Memo" value={form.memo} onChange={(v) => set({ memo: v })} />
              </Field>

              {lines && (
                <VStack gap={1}>
                  <Text weight="semibold">Lines</Text>
                  {lines.map((l, i) => (
                    <HStack key={i} hAlign="between" gap={3}>
                      <Text>{l.description}</Text>
                      <Text hasTabularNumbers>{money(l.amount_cents)}</Text>
                    </HStack>
                  ))}
                </VStack>
              )}
              {latest?.bill_input?.lines && !lines && (
                <Text type="supporting">
                  The amount was changed, so the lines read from the document no longer add up and the bill is
                  entered without them.
                </Text>
              )}

              {duplicate && (
                <Banner
                  status="warning"
                  title="This looks like a bill you already have"
                  description={duplicate}
                  endContent={<Button label="It's a separate bill" size="sm" clickAction={() => create(true)} />}
                />
              )}

              <HStack gap={2}>
                <Button label="Create bill" variant="primary" isDisabled={!canCreate} clickAction={() => create()} />
                {x && <Button label="Read again" variant="ghost" clickAction={readAgain} />}
                <Button label="Not a bill" variant="ghost" onClick={() => setTrashing(true)} />
              </HStack>
            </VStack>
          )}

          {x?.checks && x.checks.length > 0 && (
            <VStack gap={2}>
              <Heading level={2}>Checks</Heading>
              {x.checks.map((c) => (
                <HStack key={c.check} gap={2} vAlign="start">
                  <Badge label={c.ok ? 'OK' : 'Check'} variant={c.ok ? 'success' : 'warning'} />
                  <Text type="supporting">{c.detail}</Text>
                </HStack>
              ))}
            </VStack>
          )}
        </VStack>
      </div>
      <div className="payvy-split-doc">
        <DocumentViewer documentId={doc.id} contentType={doc.content_type} highlights={highlights} focus={focus} />
      </div>

      {trashing && (
        <ReasonDialog
          title="Not a bill"
          subtitle="It leaves the inbox; the file is kept."
          actionLabel="Set aside"
          isRequired
          onClose={() => setTrashing(false)}
          onSubmit={async (reason) => {
            await run('document.trash', { document_id: doc.id, reason });
            toast({ body: `${doc.filename} set aside` });
            navigate('/documents');
          }}
        />
      )}
    </div>
  );
}

/** Focusing a form field finds the value it came from on the page. */
function Field({
  focusKey,
  onFocus,
  children,
}: {
  focusKey: string;
  onFocus: (key: string) => void;
  children: ReactNode;
}) {
  return (
    <div style={{ flex: 1, minWidth: 0 }} onFocusCapture={() => onFocus(focusKey)}>
      {children}
    </div>
  );
}
