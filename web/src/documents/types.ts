// What document.get returns, and the fields shared by every page that shows
// a document next to what was read from it.
import type { Highlight } from '../viewer/DocumentViewer';
import { dateForms } from '../viewer/match';

export interface Doc {
  id: string;
  filename: string;
  content_type: string;
  size_bytes: number;
  source: string;
  status: 'inbox' | 'filed' | 'trashed';
  bill_id: string | null;
  trashed_reason: string | null;
  received_at: string;
  created_by: string;
}

/** What the model read, as printed on the document. */
export interface Read {
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
  ready: boolean;
  failed_checks: string[];
  bill_input: BillInput | null;
  new_vendor: { name: string; email: string | null } | null;
}

export interface DocumentDetail {
  document: Doc;
  /** Newest first. */
  extractions: ExtractionSummary[];
}

export const FIELDS: { key: keyof Read; label: string; isDate?: boolean; isAmount?: boolean }[] = [
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
