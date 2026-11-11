import { useCallback, useEffect, useMemo, useRef, useState, type ComponentProps, type ReactNode } from 'react';
import { useNavigate, useParams } from 'react-router';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Collapsible } from '@astryxdesign/core/Collapsible';
import { DateInput } from '@astryxdesign/core/DateInput';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { Selector } from '@astryxdesign/core/Selector';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { DocumentLoading, readWithin } from '../documents/Loading';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { ApiError, run } from '../api/client';
import { ReasonDialog } from '../inbox/ReasonDialog';
import { cents, date, dollars, money } from '../money';
import { DocumentViewer } from '../viewer/DocumentViewer';
import {
  highlightsOf,
  type BillLine,
  type DocumentDetail,
  type ExtractionSummary,
  type SuggestedInvoice,
} from '../documents/types';
import { PageStrip } from '../documents/PageStrip';
import { describePages, historyLine, PAGE_EVENTS } from '../documents/pages';
import { useSession } from '../session';

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


/** What each extraction check looks at, in words. */
const CHECK: Record<string, string> = {
  is_invoice: 'It is an invoice',
  vendor: 'Vendor',
  total: 'Total',
  invoice_number: 'Invoice number',
  invoice_date: 'Invoice date',
  invoice_date_readable: 'Invoice date',
  due_date_readable: 'Due date',
  due_after_invoice: 'Due after the invoice date',
  currency: 'Currency',
  lines_readable: 'Line items',
  lines_add_up: 'Line items add up',
  subtotal_plus_tax: 'Subtotal plus tax is the total',
  not_a_duplicate: 'Possible duplicate',
  one_invoice: 'One invoice',
};

/** A check's detail for a person: the server also writes it for agents. */
function checkDetail(c: { check: string; detail: string }, newVendor: string | undefined): string {
  if (c.check === 'not_a_duplicate') return 'This vendor already has a bill with this invoice number. Check it before creating another.';
  if (c.check === 'vendor' && newVendor) return `${newVendor} isn't one of your vendors yet. Add it, or choose another.`;
  if (c.check === 'vendor') return c.detail.replace(/; pick one$/, ': choose one.');
  return c.detail;
}

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

/** The pages a person can tick: the visible ones, in the order shown. */
function visiblePages(d: DocumentDetail): number[] {
  return (d.pages.arrangement ?? []).filter((p) => !p.hidden).map((p) => p.page);
}

/**
 * The invoices the reader found that are still here: each group whose pages
 * are all still this document's. Two or more is a file to split.
 */
function invoicesHere(d: DocumentDetail): SuggestedInvoice[] {
  const visible = visiblePages(d);
  const found = d.extractions[0]?.invoices ?? [];
  return found.filter((g) => g.pages.length > 0 && g.pages.every((p) => visible.includes(p)));
}

/** "Invoice 85273", or its place when no number was read. */
const invoiceName = (g: SuggestedInvoice, i: number) => (g.invoice_number ? `Invoice ${g.invoice_number}` : `Invoice ${i + 1}`);

/**
 * One inbox document beside what was read from it: correct anything that's
 * wrong, then make it a bill. Corrections are recorded against the reading.
 */
export function DocumentReview() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const toast = useToast();
  const { me } = useSession();
  const [detail, setDetail] = useState<DocumentDetail | null>(null);
  const [vendors, setVendors] = useState<Vendor[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<Form>(EMPTY);
  const [focus, setFocus] = useState<string | null>(null);
  const [duplicate, setDuplicate] = useState<string | null>(null);
  const [trashing, setTrashing] = useState(false);
  const [mobileView, setMobileView] = useState('details');
  const [reading, setReading] = useState(false);
  /** Pages ticked for the bill: the original's page numbers. */
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const request = useRef(0);
  const duplicateNotice = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (duplicate) duplicateNotice.current?.focus();
  }, [duplicate]);

  const load = useCallback(async () => {
    if (!id) return;
    const current = ++request.current;
    setError(null);
    try {
      const [d, v] = await readWithin(Promise.all([
        run<DocumentDetail>('document.get', { document_id: id }),
        run<{ vendors: Vendor[] }>('vendor.list', {}),
      ]));
      if (current !== request.current) return;
      setDetail(d.result);
      setVendors(v.result.vendors);
      // A file of several invoices starts with the first one ticked.
      const groups = invoicesHere(d.result);
      const first = groups.length > 1 ? groups[0] : undefined;
      const form = suggested(d.result.extractions[0]);
      setForm(first ? withInvoice(form, first) : form);
      setSelected(new Set(first ? first.pages : visiblePages(d.result)));
      setError(null);
    } catch (e) {
      if (current === request.current) setError(e instanceof Error ? e.message : String(e));
    }
  }, [id]);
  useEffect(() => {
    setDetail(null);
    void load();
    return () => { request.current++; };
  }, [load]);

  /** The document again, keeping what was typed in the form. */
  const refresh = useCallback(async () => {
    if (!id) return;
    try {
      const d = await run<DocumentDetail>('document.get', { document_id: id });
      setDetail(d.result);
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }, [id, toast]);

  /** The pages as they are now, keeping the ticks on pages still shown. */
  const arranged = useCallback((d: DocumentDetail) => {
    setDetail(d);
    const visible = visiblePages(d);
    setSelected((s) => new Set([...s].filter((p) => visible.includes(p))));
  }, []);

  const latest = detail?.extractions[0];
  const highlights = useMemo(() => highlightsOf(latest?.extraction.answer), [latest]);

  if (error) return <div className="payvy-review-error"><Link href="/documents">← Documents</Link><h1>Review invoice</h1><Banner status="error" title="Couldn't load this document" description={error} /><Button label="Try again" variant="secondary" onClick={() => void load()} /></div>;
  if (!detail) return <DocumentLoading />;

  const doc = detail.document;
  const x = latest?.extraction;
  const amountCents = cents(form.amount);
  const visible = visiblePages(detail);
  const picked = visible.filter((p) => selected.has(p));
  // Some pages, not all: they become their own document, filed with the bill.
  const partial = visible.length > 1 && picked.length > 0 && picked.length < visible.length;
  const groups = doc.status === 'inbox' ? invoicesHere(detail) : [];
  // The suggested lines only add up while the amount is the one they were read with.
  const lines: BillLine[] | undefined =
    latest?.bill_input?.lines && amountCents === latest.bill_input.amount_cents ? latest.bill_input.lines : undefined;
  const canCreate =
    doc.status === 'inbox' &&
    !!form.vendor_id &&
    !!form.invoice_number.trim() &&
    !!form.invoice_date &&
    !!amountCents &&
    (visible.length <= 1 || picked.length > 0);
  const createLabel = partial ? `Create bill from ${picked.length} ${picked.length === 1 ? 'page' : 'pages'}` : 'Create bill';
  const set = (patch: Partial<Form>) => setForm((f) => ({ ...f, ...patch }));

  async function readAgain() {
    setReading(true);
    try {
      await run('document.extract', { document_id: doc.id, again: true });
      await load();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    } finally { setReading(false); }
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
        ...(partial ? { pages: picked, expected_version: detail!.pages.version } : {}),
      });
      toast({ body: partial ? 'Bill created. The other pages stay in To review.' : 'Bill created' });
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

  /** Tick one of the invoices the reader found, with its number and total. */
  function pickInvoice(g: SuggestedInvoice) {
    setSelected(new Set(g.pages));
    setForm((f) => withInvoice(f, g));
  }

  async function undoSplit() {
    const from = detail?.split_from;
    if (!from) return;
    try {
      await run('document.unsplit', { document_id: doc.id });
      toast({ body: `Pages put back in ${from.filename ?? 'the document they came from'}` });
      navigate(`/documents/${from.id}`);
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  const checks = x?.checks ?? [];
  // A vendor problem is settled once someone chooses or adds the vendor;
  // several invoices have their own notice below.
  const open = checks.filter(
    (c) => !c.ok && !(c.check === 'vendor' && form.vendor_id) && !(c.check === 'one_invoice' && groups.length > 1),
  );
  const passed = checks.filter((c) => c.ok);
  const missing = [
    !form.vendor_id && 'choose a vendor',
    !form.invoice_number.trim() && 'enter the invoice number',
    !form.invoice_date && 'enter the invoice date',
    !amountCents && 'enter the amount',
  ].filter((m): m is string => !!m);

  const nameOf = (actor: string) => (actor === me.person.actor ? me.person.name : actor);
  const pageHistory = detail.history.filter((h) => PAGE_EVENTS.includes(h.kind)).slice(0, 5);
  const parts = detail.split_into.filter((p) => p.pages.length > 0);

  const match = x?.vendor_match;
  const vendorNote =
    match?.result === 'matched'
      ? `Matched ${match.name}: ${match.reasons.join('; ')}`
      : match?.result === 'ambiguous'
        ? `More than one vendor fits: ${match.vendor_ids
            .map((v) => vendors.find((x) => x.id === v)?.name ?? v)
            .join(', ')}. Choose one.`
        : undefined;

  return (
    <div className="payvy-document-review" data-mobile-view={mobileView}>
      <header className="payvy-review-header">
        <div><Link href="/documents">← Documents</Link><h1>Review invoice</h1><p className="payvy-review-filename" title={doc.filename}>{doc.filename}</p></div>
        <span className="payvy-document-status">
          {doc.status === 'filed'
            ? 'Filed'
            : doc.status === 'trashed'
              ? 'Not a bill'
              : doc.status === 'split'
                ? 'Split'
                : 'To review'}
        </span>
      </header>
      <div className="payvy-review-tabs"><SegmentedControl label="Document view" value={mobileView} onChange={setMobileView}>
        <SegmentedControlItem value="details" label="Invoice details" /><SegmentedControlItem value="original" label="Original document" />
      </SegmentedControl></div>
      <div className="payvy-review-workspace">
      <section className="payvy-review-side" aria-label="Invoice details">
        <div className="payvy-review-body"><VStack gap={5}>
          <div className="payvy-invoice-summary">
            <span className="payvy-eyebrow">Invoice details</span>
            <Heading level={2}>{x?.draft?.vendor_name ?? 'Your invoice'}</Heading>
            {amountCents !== null && amountCents > 0 && <strong className="payvy-invoice-amount">{money(amountCents)}</strong>}
            <p>Compare these details with the original before creating a bill.</p>
          </div>

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
          {doc.status === 'split' && (
            <Banner
              status="info"
              title="Its pages were moved to other documents"
              description="Nothing is left to review here. The original file is kept."
            />
          )}
          {detail.split_from && (
            <Banner
              status="info"
              title={`Split from ${detail.split_from.filename ?? 'another document'}`}
              description={
                <>
                  <Link href={`/documents/${detail.split_from.id}`}>Open the rest of the file</Link>
                  {doc.status === 'inbox' && (
                    <span className="payvy-split-note">Undo puts these pages back where they came from.</span>
                  )}
                </>
              }
              endContent={
                doc.status === 'inbox' ? (
                  <Button label="Undo" size="sm" variant="secondary" clickAction={undoSplit} />
                ) : undefined
              }
            />
          )}
          {groups.length > 1 && !reading && (
            <Banner
              status="info"
              title={`This file has ${groups.length} invoices`}
              description={
                <VStack gap={2}>
                  <span>Tick the pages of one invoice and create its bill. The other pages stay here.</span>
                  <ul className="payvy-split-list" aria-label="Invoices in this file">
                    {groups.map((g, i) => (
                      <li key={g.pages.join('-')}>
                        <span>
                          <strong>{invoiceName(g, i)}</strong> · {describePages(g.pages)}
                          {g.total_cents !== null && ` · ${money(g.total_cents)}`}
                        </span>
                        <Button
                          label={`Select ${invoiceName(g, i)}`}
                          size="sm"
                          variant="ghost"
                          isDisabled={picked.length === g.pages.length && g.pages.every((p) => selected.has(p))}
                          onClick={() => pickInvoice(g)}
                        >
                          Select
                        </Button>
                      </li>
                    ))}
                  </ul>
                </VStack>
              }
            />
          )}
          {parts.length > 0 && (
            <VStack gap={2}>
              <Text type="label">Split into</Text>
              <ul className="payvy-split-list" aria-label="Split into">
                {parts.map((p) => (
                  <li key={p.id}>
                    <Link href={`/documents/${p.id}`}>{p.filename}</Link>
                    <Text type="supporting">{p.status === 'filed' ? 'Filed' : p.status === 'trashed' ? 'Not a bill' : 'To review'}</Text>
                  </li>
                ))}
              </ul>
            </VStack>
          )}

          {reading && <div role="status" className="payvy-reading-notice">Reading the invoice… Your original stays available while we check the details.</div>}
          {!x && !reading && doc.status === 'inbox' && (
            <Banner
              status="info"
              title="Not read yet"
              description="Read it with the model, or fill in the bill yourself."
              endContent={<Button label="Read it" size="sm" clickAction={readAgain} />}
            />
          )}
          {x?.status === 'failed' && !reading && (
            <Banner
              status="error"
              title="Reading it failed"
              description={x.error ?? undefined}
              endContent={<Button label="Try again" size="sm" clickAction={readAgain} />}
            />
          )}
          {latest?.out_of_date && x?.status === 'read' && !reading && doc.status === 'inbox' && (
            <Banner
              status="warning"
              title="Read before the pages were rearranged"
              description="These details were read from the pages as they were. Read it again to bring them up to date."
              endContent={<Button label="Read again" size="sm" clickAction={readAgain} />}
            />
          )}
          {doc.status === 'inbox' && open.length > 0 && (
            <Banner
              status="warning"
              title={open.length === 1 ? "One thing to check" : `${open.length} things to check`}
              description={
                <ul className="payvy-issues">
                  {open.map((c) => (
                    <li key={c.check}>
                      <strong>{CHECK[c.check] ?? c.check}:</strong> {checkDetail(c, latest?.new_vendor?.name)}
                    </li>
                  ))}
                </ul>
              }
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
                <Button
                  label={`Add ${latest.new_vendor.name} as a vendor`}
                  size="sm"
                  variant="secondary"
                  clickAction={addVendor}
                />
              )}
              <Field focusKey="invoice_number" onFocus={setFocus}>
                <TextInput
                  label="Invoice number"
                  isRequired
                  value={form.invoice_number}
                  onChange={(v) => set({ invoice_number: v })}
                />
              </Field>
              <div className="payvy-review-dates">
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
                    value={iso(form.due_date)}
                    onChange={(v) => set({ due_date: v ?? '' })}
                  />
                </Field>
              </div>
              {!form.due_date && (
                <Text type="supporting">No due date: the vendor's payment terms decide it.</Text>
              )}
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
                  <Text type="label">Lines</Text>
                  {lines.map((l, i) => (
                    <HStack key={i} hAlign="between" gap={3}>
                      <Text>{l.description}</Text>
                      <Text hasTabularNumbers>{money(l.amount_cents)}</Text>
                    </HStack>
                  ))}
                  {amountCents !== null && (
                    <HStack hAlign="between" gap={3} className="payvy-total">
                      <Text weight="semibold">Total</Text>
                      <Text weight="semibold" hasTabularNumbers>
                        {money(amountCents)}
                      </Text>
                    </HStack>
                  )}
                </VStack>
              )}
              {latest?.bill_input?.lines && !lines && (
                <Text type="supporting">
                  The amount was changed, so the lines read from the document no longer add up and the bill is
                  entered without them.
                </Text>
              )}

              {duplicate && (
                <div ref={duplicateNotice} tabIndex={-1} role="alert">
                <Banner
                  status="warning"
                  title="This looks like a bill you already have"
                  description="This vendor already has a bill with this invoice number. Only continue if these are separate invoices."
                  endContent={<Button label="It's a separate bill" size="sm" clickAction={() => create(true)} />}
                />
                </div>
              )}


            </VStack>
          )}

          {passed.length > 0 && (
            <Collapsible
              defaultIsOpen={false}
              trigger={
                <Text type="supporting">
                  ✓ {passed.length} check{passed.length === 1 ? '' : 's'} passed
                </Text>
              }
            >
              <ul className="payvy-issues payvy-passed">
                {passed.map((c) => (
                  <li key={c.check}>
                    {CHECK[c.check] ?? c.check}
                    {c.detail && c.check !== 'is_invoice' ? <Text type="supporting"> · {c.detail}</Text> : null}
                  </li>
                ))}
              </ul>
            </Collapsible>
          )}
          {pageHistory.length > 0 && (
            <VStack gap={2}>
              <Text type="label">Page history</Text>
              <ol className="payvy-page-history" aria-label="Page history">
                {pageHistory.map((h) => (
                  <li key={h.id}>
                    <Text>{historyLine(h, nameOf)}</Text>
                    <Text type="supporting">{date(h.at)}</Text>
                  </li>
                ))}
              </ol>
            </VStack>
          )}
          <details className="payvy-document-provenance"><summary>Document information</summary><p>{doc.source === 'email' ? 'Emailed' : 'Uploaded'} {date(doc.received_at)}</p>{x && <p>Read by {x.model}</p>}</details>
        </VStack></div>
        {doc.status === 'inbox' && <footer className="payvy-review-actions">
          <div className="payvy-review-primary"><div><strong>{!canCreate ? 'Complete the details' : open.length ? 'Review the checks above' : 'Ready for your review'}</strong><p>{missing.length
                  ? `To create the bill, ${missing.join(' and ')}.`
                  : visible.length > 1 && picked.length === 0
                    ? 'Tick the pages for this bill.'
                    : partial
                      ? `${describePages(picked)} ${picked.length === 1 ? 'becomes' : 'become'} a bill. The other pages stay in To review.`
                      : 'The bill will follow your workflow.'}</p></div><Button
              label={createLabel}
              variant="primary"
              isDisabled={!canCreate || reading}
              clickAction={() => create()}
            /></div>
          <div className="payvy-review-secondary">{x && <Button label="Read again" variant="ghost" size="sm" isDisabled={reading} clickAction={readAgain} />}<Button label="Not a bill" variant="ghost" size="sm" isDisabled={reading} onClick={() => setTrashing(true)} /></div>
        </footer>}
      </section>
      <section className="payvy-review-original" aria-label="Original document">
        <PageStrip
          documentId={doc.id}
          contentType={doc.content_type}
          pages={detail.pages}
          onChange={arranged}
          onStale={refresh}
          {...(doc.status === 'inbox' && !detail.pages.locked ? { selected, onSelect: setSelected } : {})}
        />
        <div className="payvy-review-viewer">
          <DocumentViewer
            documentId={doc.id}
            contentType={doc.content_type}
            highlights={highlights}
            focus={focus}
            arrangement={detail.pages.arrangement}
          />
        </div>
      </section>
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

/** The form for one of several invoices: its number and total, where read. */
function withInvoice(form: Form, g: SuggestedInvoice): Form {
  return {
    ...form,
    ...(g.invoice_number ? { invoice_number: g.invoice_number } : {}),
    ...(g.total_cents ? { amount: dollars(g.total_cents) } : {}),
  };
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

