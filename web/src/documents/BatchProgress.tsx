// What became of each invoice after "Create N bills": read and made bills
// on the server, so this only shows progress. The page polls the document
// while any part is still being read, and shows it again when reopened.
import { Link } from '@astryxdesign/core/Link';
import { money } from '../money';
import { describePages } from './pages';
import type { BillBatch, BillBatchPart } from './types';

const name = (p: BillBatchPart) => (p.invoice_number ? `Invoice ${p.invoice_number}` : `Invoice ${p.position + 1}`);

function line(p: BillBatchPart) {
  switch (p.state) {
    case 'reading':
      return <span>Reading…</span>;
    case 'created':
      return p.bill_id ? <Link href={`/bills/${p.bill_id}`}>Bill created</Link> : <span>Bill created</span>;
    case 'stacked':
      return p.bill_id ? <Link href={`/bills/${p.bill_id}`}>{p.note ?? 'Stacked on an existing bill'}</Link> : <span>{p.note}</span>;
    case 'needs_you':
      return <Link href={`/inbox/${p.document_id}`}>{p.note ?? 'Needs you'}</Link>;
    case 'withdrawn':
      return <span>{p.note ?? 'Handled by a person'}</span>;
  }
}

export function summary(b: BillBatch): string {
  const c = b.counts;
  return [
    c.created && `${c.created} ${c.created === 1 ? 'bill' : 'bills'} created`,
    c.stacked && `${c.stacked} stacked on existing bills`,
    c.needs_you && `${c.needs_you} ${c.needs_you === 1 ? 'needs' : 'need'} you`,
    c.withdrawn && `${c.withdrawn} handled by a person`,
    c.reading && `${c.reading} still reading`,
  ]
    .filter(Boolean)
    .join(' · ');
}

export function BatchProgress({ batch }: { batch: BillBatch }) {
  const n = batch.parts.length;
  const finished = n - batch.counts.reading;
  return (
    <section className="payvy-batch" aria-label="Bills from this file" data-done={batch.done || undefined}>
      <header>
        <strong>{batch.done ? `Bills from this file` : `Creating ${n} bills…`}</strong>
        <span className="payvy-muted">
          {batch.done ? summary(batch) : `${finished} of ${n} done · ${summary(batch)}. You can leave this page; it carries on.`}
        </span>
        {!batch.done && <progress max={n} value={finished} aria-label="Bills created so far" />}
      </header>
      {batch.done ? (
        <details className="payvy-batch-more">
          <summary>Each invoice</summary>
          <ol className="payvy-batch-parts" aria-label="Results">
            {batch.parts.map((p) => (
              <li key={p.position} data-state={p.state}>
                <span>
                  <strong>{name(p)}</strong>{' '}
                  <span className="payvy-muted">
                    {describePages(p.pages)}
                    {p.total_cents != null && ` · ${money(p.total_cents)}`}
                  </span>
                </span>
                {line(p)}
              </li>
            ))}
          </ol>
        </details>
      ) : (
        <ol className="payvy-batch-parts" aria-label="Results">
          {batch.parts.map((p) => (
            <li key={p.position} data-state={p.state}>
              <span>
                <strong>{name(p)}</strong>{' '}
                <span className="payvy-muted">
                  {describePages(p.pages)}
                  {p.total_cents != null && ` · ${money(p.total_cents)}`}
                </span>
              </span>
              {line(p)}
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
