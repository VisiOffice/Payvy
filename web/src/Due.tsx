// A due date with its urgency in words, when it's close or past.
// Far-off dates say nothing here; the date itself is shown beside it.
import { dueLabel } from './money';

export function DueText({ iso }: { iso: string | null | undefined }) {
  const due = dueLabel(iso);
  if (!due) return null;
  return <span className={due.tone === 'bad' ? 'payvy-due-bad' : 'payvy-due-soon'}>{due.text}</span>;
}
