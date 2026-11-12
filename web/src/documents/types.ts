// What document.get returns, and the fields shared by every page that shows
// a document next to what was read from it.
import type { Highlight } from '../viewer/DocumentViewer';
import { dateForms } from '../viewer/match';
import type { DocumentEvent, Pages } from './pages';

export interface Doc {
  id: string;
  filename: string;
  content_type: string;
  size_bytes: number;
  source: string;
  /** split: every page moved to other documents. */
  status: 'inbox' | 'filed' | 'trashed' | 'split';
  bill_id: string | null;
  trashed_reason: string | null;
  received_at: string;
  created_by: string;
  page_count: number | null;
  /** The document this one was split from; it shares that original file. */
  derived_from: string | null;
}

/** Where a document came from, as document.get and document.list name it. */
export interface SplitFrom {
  id: string;
  filename: string | null;
}

/** A document split from this one, with its pages (the original's numbers). */
export interface SplitPart {
  id: string;
  filename: string;
  status: Doc['status'];
  bill_id: string | null;
  pages: number[];
}

/** One of several invoices a reading found in the file. */
export interface SuggestedInvoice {
  /** Page numbers of the original, in the document's order. */
  pages: number[];
  invoice_number: string | null;
  total: string | null;
  total_cents: number | null;
}

/** What the model read, as printed on the document. */
export interface Read {
  lines?: {description:string; amount:string}[];
  vendor_name: string | null;
  invoice_number: string | null;
  invoice_date: string | null;
  due_date: string | null;
  po_number: string | null;
  account_number: string | null;
  terms: string | null;
  subtotal: string | null;
  tax: string | null;
  total: string | null;
}

export interface Check {
  check: string;
  ok: boolean;
  detail: string;
}

export type VendorMatch =
  | { result: 'matched'; vendor_id: string; name: string; confidence: string; reasons: string[] }
  | { result: 'ambiguous'; vendor_ids: string[] }
  | { result: 'no_match' };

export interface Draft {
  vendor_name: string | null;
  invoice_number: string | null;
  invoice_date: string | null;
  due_date: string | null;
  po_number: string | null;
  total_cents: number | null;
}

export interface BillLine {
  description: string;
  amount_cents: number;
  account?: string | null;
}

export interface BillInput {
  vendor_id: string;
  invoice_number: string;
  invoice_date: string;
  due_date?: string;
  amount_cents: number;
  memo?: string;
  lines?: BillLine[];
  document_id: string;
  extraction_id: string;
}

export interface ExtractionSummary {
  extraction: {
    id: string;
    status: 'read' | 'failed';
    provider: string;
    model: string;
    answer: Read | null;
    draft: Draft | null;
    checks: Check[] | null;
    vendor_match: VendorMatch | null;
    error: string | null;
    created_at: string;
  };
  /** Several invoices in one file: where each is. Empty for one. */
  invoices: SuggestedInvoice[];
  ready: boolean;
  failed_checks: string[];
  bill_input: BillInput | null;
  new_vendor: { name: string; email: string | null } | null;
  /** Read before the pages were last rearranged. */
  out_of_date?: boolean;
}

export interface DocumentDetail {
  document: Doc;
  pages: Pages;
  /** Newest first. */
  extractions: ExtractionSummary[];
  /** Everything that happened to the document, newest first. */
  history: DocumentEvent[];
  split_from: SplitFrom | null;
  /** Documents split from this one, in page order. */
  split_into: SplitPart[];
  /** The newest "Create N bills" made from this document, if any. */
  bill_batch?: BillBatch | null;
}

/** One invoice of a batch, and what became of it. */
export interface BillBatchPart {
  position: number;
  document_id: string;
  filename: string | null;
  pages: number[];
  invoice_number: string | null;
  total_cents: number | null;
  state: 'reading' | 'created' | 'stacked' | 'needs_you' | 'withdrawn';
  bill_id: string | null;
  note: string | null;
}

/** "Create N bills" on a file of several invoices: split at once, each part read and billed on the server. */
export interface BillBatch {
  id: string;
  document_id: string;
  request_id: string;
  requested_by: string;
  requested_at: string;
  done: boolean;
  counts: Record<BillBatchPart['state'], number>;
  parts: BillBatchPart[];
}

export const FIELDS: { key: Exclude<keyof Read, 'lines'>; label: string; isDate?: boolean; isAmount?: boolean }[] = [
  { key: 'vendor_name', label: 'Vendor' },
  { key: 'invoice_number', label: 'Invoice number' },
  { key: 'invoice_date', label: 'Invoice date', isDate: true },
  { key: 'due_date', label: 'Due date', isDate: true },
  { key: 'total', label: 'Total', isAmount: true },
  { key: 'subtotal', label: 'Subtotal', isAmount: true },
  { key: 'tax', label: 'Tax', isAmount: true },
  { key: 'po_number', label: 'PO number' },
  { key: 'account_number', label: 'Account number' },
  { key: 'terms', label: 'Terms' },
];

function candidates(value: string, f: (typeof FIELDS)[number]): string[] {
  if (f.isDate) return dateForms(value);
  if (f.isAmount) {
    const bare = value.replace(/^\$\s*/, '');
    return [value, `$${bare}`, bare];
  }
  return [value];
}

/** Where each value read from the document should be found on it. */
export function highlightsOf(read: Read | null | undefined): Highlight[] {
  if (!read) return [];
  return FIELDS.flatMap((f) => {
    const v = read[f.key];
    return v ? [{ key: f.key, label: f.label, candidates: candidates(v, f) }] : [];
  });
}
