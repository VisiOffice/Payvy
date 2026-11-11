import { Button } from '@astryxdesign/core/Button';
import { TextInput } from '@astryxdesign/core/TextInput';
import { Selector } from '@astryxdesign/core/Selector';
import { CheckboxInput } from '@astryxdesign/core/CheckboxInput';
import { Banner } from '@astryxdesign/core/Banner';
import { VStack } from '@astryxdesign/core/VStack';
import { HStack } from '@astryxdesign/core/HStack';
import { Text } from '@astryxdesign/core/Text';
import { Collapsible } from '@astryxdesign/core/Collapsible';
import { useState } from 'react';
import { money } from '../money';
import type { CommandInputs } from '../api/commands.gen';

export type ReviewPayload = CommandInputs['document.review_save']['payload'];
export interface ReviewValidation {
  ready: boolean; header_ready: boolean; total_cents: number | null;
  sum_cents: number | null; difference_cents: number | null;
  issues: { code: string; row_id: string | null; detail: string }[];
}
export interface ReviewRevision {
  id: string; version: number; arrangement_version: number; extraction_id: string | null;
  payload: ReviewPayload; summary_reason: string | null; created_by: string; created_at: string;
}
export interface SourceEvidence { field: string; page: number | null; quote: string | null; status: string }
export interface ReviewAnalysis { schema_version: number; coverage_ok?: boolean; coverage?: {reason?: string | null}; rows?: SourceEvidence[][] }
export interface ReviewDetail {
  analysis?: ReviewAnalysis | null;
  enabled: boolean; required: boolean; review: ReviewRevision | null;
  history: ReviewRevision[]; initial: ReviewPayload; extraction_id: string | null;
  arrangement_version: number; validation: ReviewValidation;
}

/** A live display only. The server performs authoritative reconciliation. */
function signedCents(s: string): number | null {
  const raw = s.trim().replace(/^\$\s*/, '').replace(/\s*USD$/, '').trim();
  const t = (raw.startsWith('(') && raw.endsWith(')') ? '-' + raw.slice(1,-1) : raw).replaceAll(',', '');
  const m = /^(-?)(\d+)(?:\.(\d{1,2}))?$/.exec(t);
  if (!m) return null;
  const v = (Number(m[2]) * 100 + Number((m[3] ?? '').padEnd(2, '0'))) * (m[1] ? -1 : 1);
  return Number.isSafeInteger(v) ? v : null;
}
export function LineReview({ value, onChange, detail, dirty, busy, save, summary, canSummary, source, newer, useNew, partial, split, evidence }: {
  value: ReviewPayload; onChange: (p: ReviewPayload) => void; detail: ReviewDetail;
  dirty: boolean; busy: boolean; save: () => Promise<void>; summary: (reason: string) => Promise<void>;
  evidence: (item: SourceEvidence) => void;
  canSummary: boolean; source: (index: number) => void; newer: boolean; useNew: () => void; partial: boolean; split: (() => Promise<void>) | null;
}) {
  const [reason, setReason] = useState('');
  const [summaryOpen, setSummaryOpen] = useState(false);
  const rows = value.rows;
  const richer = detail.analysis?.schema_version === 2;
  function patch(index: number, change: Partial<ReviewPayload['rows'][number]>) {
    if ('amount' in change || 'description' in change || 'details' in change) {
      change = {...change, quantity_resolution:null, source_resolution:null};
    }
    onChange({ ...value, rows: rows.map((r,i) => i === index ? { ...r, ...change } : r) });
  }
  const amounts = rows.filter(r => r.excluded_reason === null).map(r => signedCents(r.amount));
  const tax = value.tax_basis === 'additional' ? signedCents(value.tax) : 0;
  const sum = amounts.every(a => a !== null) && tax !== null ? amounts.reduce<number>((a,b) => a + b!,0) + tax : null;
  const total = signedCents(value.total);
  const difference = sum !== null && total !== null ? total - sum : null;
  const summaryAccepted = !dirty && !!detail.review?.summary_reason;
  return <section className="payvy-line-review" aria-label="Line item review">
    <VStack gap={3}>
      <HStack hAlign="between"><Text weight="semibold">Line items</Text><Text type="supporting">{dirty ? 'Unsaved changes' : detail.review ? `Saved · revision ${detail.review.version}` : 'Not saved yet'}</Text></HStack>
      <Text type="supporting">Keep discounts negative. Exclude a row with a reason; the original stays in the record.</Text>
      {partial && <Banner status="warning" title="Split these pages first" description="Line reviews cover one document. Select the pages for one invoice and split them into a document before reviewing its lines." endContent={split ? <Button label="Split selected pages" size="sm" clickAction={split} /> : undefined} />}
      {newer && <Banner status="info" title="A newer reading is available" description="Your edits are still here. Save any changes before choosing the new reading; earlier revisions remain in history." endContent={<Button label="Use new reading" variant="secondary" size="sm" isDisabled={dirty} onClick={useNew} />} />}
      {richer && detail.analysis?.coverage_ok !== true && <VStack gap={2}>
        <Banner status="warning" title="Page coverage needs review" description={detail.analysis?.coverage?.reason || 'The reader could not establish that every selected page was read. Check the complete original or read again.'} />
        <TextInput label="Page coverage resolution" value={value.coverage_resolution ?? ''} onChange={coverage_resolution => onChange({...value,coverage_resolution})} description="Explain how you checked all selected pages and any missing rows." />
      </VStack>}
      {rows.length === 0 && <Banner status="warning" title="No line items captured" description="Add the printed lines, or review and explicitly accept a summary-only bill." />}
      {rows.map((row,index) => <div className="payvy-review-row" key={row.id} data-excluded={row.excluded_reason !== null}>
        <HStack hAlign="between"><Text type="label">Line {index+1}</Text><HStack gap={1}>
          {row.source_index !== null && <Button label={`Source line ${index+1}`} size="sm" variant="ghost" onClick={() => source(row.source_index!)} />}
          {index > 0 && <Button label={`Move up ${index+1}`} size="sm" variant="ghost" onClick={() => { const next=[...rows]; [next[index-1],next[index]]=[next[index]!,next[index-1]!]; onChange({...value, rows:next}); }} />}
        </HStack></HStack>
        <div className="payvy-review-row-fields">
          <TextInput label={`Description ${index+1}`} value={row.description} onChange={description => patch(index,{description})} />
          <TextInput label={`Line amount ${index+1}`} value={row.amount} onChange={amount => patch(index,{amount})} placeholder="0.00" />
        </div>
        {(richer || row.details) && <Collapsible defaultIsOpen={false} trigger={<Text type="label">Details and source · line {index+1}</Text>}>
          <VStack gap={2}>
            <div className="payvy-review-row-fields">
            {([['quantity','Quantity'],['unit','Unit'],['unit_price','Unit price'],['tax_amount','Printed tax'],['tax_rate','Tax rate'],['item_code','Item code'],['service_start','Service start'],['service_end','Service end']] as const).map(([key,label]) => <TextInput key={key} label={`${label} ${index+1}`} value={row.details?.[key] ?? ''} onChange={v => patch(index,{details:{...row.details,[key]:v || null}})} description={row.details?.states?.[key] ? `Reader: ${row.details.states[key]}` : undefined} />)}
            </div>
            <Text type="supporting">Printed tax is reference information, not added to the bill again. Quantities and rates retain their printed precision.</Text>
            <TextInput label={`Quantity check explanation ${index+1}`} value={row.quantity_resolution ?? ''} onChange={quantity_resolution => patch(index,{quantity_resolution})} description="If quantity × rate differs from the amount, explain a printed discount, tax, bundle or rounding." />
            <Text type="label">Source evidence{dirty ? ' · save to check edits' : ''}</Text>
            {(detail.analysis?.rows?.[detail.review ? detail.review.payload.rows.findIndex(r => r.id === row.id) : (row.source_index ?? -1)] ?? []).map((item,n) => <div key={n} className="payvy-source-evidence">
              <Text type="supporting">{item.field.replaceAll('_',' ')} · {item.status === 'located' ? 'Text located' : item.status.replaceAll('_',' ')}</Text>
              {item.quote && <Text>“{item.quote}”</Text>}
              {item.page !== null && <Button label={`Show ${item.field.replaceAll('_',' ')} on page ${item.page}`} size="sm" variant="ghost" onClick={() => evidence(item)} />}
            </div>)}
            <Text type="supporting">Text location checks existence and nearby context. Check that the value belongs to this row.</Text>
            <TextInput label={`Source resolution ${index+1}`} value={row.source_resolution ?? ''} onChange={source_resolution => patch(index,{source_resolution})} description="For missing or uncertain evidence, record what you verified against the original. Editing the row clears this explanation." />
          </VStack>
        </Collapsible>}
        {row.excluded_reason !== null ? <VStack gap={1}>
          <TextInput label={`Exclusion reason ${index+1}`} value={row.excluded_reason} onChange={excluded_reason => patch(index,{excluded_reason})} isRequired />
          <Button label={`Restore line ${index+1}`} variant="ghost" size="sm" onClick={() => patch(index,{excluded_reason:null})} />
        </VStack> : <Button label={`Exclude line ${index+1}`} variant="ghost" size="sm" onClick={() => patch(index,{excluded_reason:''})} />}
      </div>)}
      <Button label="Add line" variant="secondary" size="sm" onClick={() => onChange({...value, rows:[...rows,{id:crypto.randomUUID(),description:'',amount:'',source_index:null,excluded_reason:null}]})} />
      <Selector label="Tax treatment" value={value.tax_basis} onChange={v => onChange({...value,tax_basis:v as ReviewPayload['tax_basis']})} options={[{value:'unknown',label:'Needs review'},{value:'included',label:'Included in rows / no additional tax'},{value:'additional',label:'Add tax once to the rows'}]} />
      {value.tax_basis === 'additional' && <TextInput label="Additional tax" value={value.tax} onChange={tax => onChange({...value,tax})} />}
      <TextInput label="Invoice currency" value={value.currency} onChange={currency => onChange({...value,currency:currency.toUpperCase()})} placeholder="USD" description="Confirm from the original; only USD is supported." />
      <div className="payvy-review-reconciliation" role="status">
        <Text>{sum === null ? 'Some amounts need checking' : `Rows${value.tax_basis === 'additional' ? ' + tax' : ''}: ${money(sum)}`}</Text>
        <Text weight="semibold">{value.tax_basis === 'unknown' ? 'Choose the tax treatment' : difference === null ? 'Enter the invoice total' : difference === 0 ? 'Amounts reconcile' : `Difference: ${money(difference)}`}</Text>
        <Text type="supporting">Matching amounts do not verify what was printed.</Text>
      </div>
      <CheckboxInput label="I checked the invoice details, currency and all included pages against the original" value={value.source_checked} onChange={source_checked => onChange({...value,source_checked})} />
      {!dirty && detail.validation.issues.length > 0 && !summaryAccepted && <Banner status="warning" title="Review needed" description={<ul>{detail.validation.issues.map((i,n) => <li key={n}>{i.row_id ? `Line ${rows.findIndex(r => r.id === i.row_id)+1}: ` : ''}{i.detail}</li>)}</ul>} />}
      {summaryAccepted && <Banner status="warning" title="Summary-only accepted" description={`${detail.review!.summary_reason} — by ${detail.review!.created_by}. The bill will have no posting lines; this review is retained.`} />}
      <Button label="Save review" variant="secondary" isDisabled={busy || (!dirty && !!detail.review)} clickAction={save} />
      {canSummary && !summaryAccepted && <>
        <Button label="Use summary-only bill…" size="sm" variant="ghost" isDisabled={busy || dirty || !detail.review || !detail.validation.header_ready} onClick={() => setSummaryOpen(!summaryOpen)} />
        {summaryOpen && <VStack gap={2}>
          <Text type="supporting">The bill will retain its total but no posting lines. The original and all reviewed rows stay in history. This does not approve or release payment.</Text>
          <TextInput label="Summary-only reason" value={reason} onChange={setReason} isRequired />
          <Button label="Accept summary-only" isDisabled={busy || !reason.trim()} clickAction={async () => { await summary(reason); setSummaryOpen(false); }} />
        </VStack>}
      </>}
      {detail.history.length > 0 && <Collapsible defaultIsOpen={false} trigger={<Text type="label">Review history ({detail.history.length})</Text>}>
        {detail.history.map(r => <Collapsible key={r.id} defaultIsOpen={false} trigger={<Text>Revision {r.version} · {r.created_by}{r.summary_reason ? ' · summary-only' : ''}</Text>}>
          <VStack gap={1}><Text>{r.payload.invoice_number} · {r.payload.total} {r.payload.currency}</Text>
          {r.payload.rows.map(row => <Text key={row.id}>{row.description || '(blank)'} — {row.amount || '(missing)'}{row.excluded_reason !== null ? ` · excluded: ${row.excluded_reason}` : ''}</Text>)}
          {r.payload.coverage_resolution && <Text>Page check: {r.payload.coverage_resolution}</Text>}
          {r.payload.rows.filter(row => row.details || row.source_resolution || row.quantity_resolution).map(row => <Text key={`details-${row.id}`}>{row.description}: {Object.entries(row.details ?? {}).filter(([k,v]) => typeof v === 'string' && !['states','evidence'].includes(k)).map(([k,v]) => `${k.replaceAll('_',' ')} ${v}`).join(' · ')}{row.quantity_resolution && ` · quantity check: ${row.quantity_resolution}`}{row.source_resolution && ` · source check: ${row.source_resolution}`}</Text>)}
          {r.summary_reason && <Text>{r.summary_reason}</Text>}</VStack>
        </Collapsible>)}
      </Collapsible>}
    </VStack>
  </section>;
}
