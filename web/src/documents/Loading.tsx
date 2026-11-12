import { Spinner } from '@astryxdesign/core/Spinner';
import { Link } from '@astryxdesign/core/Link';

/** Keep the review workspace recognizable while its code/data are arriving. */
export function DocumentLoading() {
  return <div className="payvy-document-review" aria-busy="true">
    <header className="payvy-review-header">
      <div><Link href="/inbox">← Inbox</Link><h1>Review invoice</h1></div>
      <span className="payvy-document-status">Opening document</span>
    </header>
    <div className="payvy-review-workspace payvy-review-loading">
      <div className="payvy-review-body">
        <div role="status" className="payvy-loading-message"><Spinner size="sm" /><div><strong>Loading the document</strong><p>Getting the original and its invoice details.</p></div></div>
        <div aria-hidden="true" className="payvy-review-placeholder">
          <span className="payvy-skeleton payvy-skeleton-title" />
          <span className="payvy-skeleton payvy-skeleton-total" />
          {[0, 1, 2, 3].map(n => <div key={n}><span className="payvy-skeleton payvy-skeleton-label" /><span className="payvy-skeleton payvy-skeleton-input" /></div>)}
        </div>
      </div>
      <div className="payvy-preview-placeholder" aria-hidden="true"><div className="payvy-paper-placeholder"><span /><span /><span /><hr /><span /><span /></div></div>
    </div>
  </div>;
}

/** Bound read-only page loads. Late responses cannot replace the retry state. */
export async function readWithin<T>(promise: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  try {
    return await Promise.race([promise, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error('The document service is taking longer than expected. Please try again.')), 15_000);
    })]);
  } finally { clearTimeout(timer!); }
}
