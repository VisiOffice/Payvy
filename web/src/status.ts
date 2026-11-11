// Plain words for where a bill stands and what happened to it.
type Variant = 'neutral' | 'info' | 'success' | 'warning' | 'error';

const STATUS: Record<string, { label: string; variant: Variant }> = {
  awaiting_approval: { label: 'Needs approval', variant: 'warning' },
  awaiting_release: { label: 'Ready to pay', variant: 'info' },
  ready_to_pay: { label: 'Pays automatically', variant: 'info' },
  held: { label: 'On hold', variant: 'neutral' },
  waiting_for: { label: 'Waiting', variant: 'neutral' },
  released: { label: 'Released', variant: 'neutral' },
  paid: { label: 'Paid', variant: 'success' },
  rejected: { label: 'Rejected', variant: 'error' },
  void: { label: 'Void', variant: 'neutral' },
};

/** A bill's evaluated status (or its closed state) as a badge. */
export function billStatus(s: string): { label: string; variant: Variant } {
  return STATUS[s] ?? { label: sentence(s), variant: 'neutral' };
}

const EVENT: Record<string, string> = {
  'notification.queued': 'Email notification queued',
  'notification.sending': 'Email sending started',
  'notification.delivery': 'Email delivery updated',
  'bill.created': 'Entered',
  'bill.approved': 'Approved',
  'bill.rejected': 'Rejected',
  'bill.released': 'Released for payment',
  'bill.auto_released': 'Paid automatically by its workflow',
  'bill.signalled': 'Signal received',
  'bill.paid': 'Paid',
  'bill.payment_failed': 'Payment failed',
  'bill.payment_returned': 'Payment returned',
  'bill.voided': 'Voided',
  'bill.pages_arranged': 'Pages rearranged',
};

/** An event kind like `bill.created` as words. */
export function eventLabel(kind: string): string {
  return EVENT[kind] ?? sentence(kind.split('.').pop() ?? kind);
}

function sentence(s: string): string {
  const t = s.replaceAll('_', ' ');
  return t.charAt(0).toUpperCase() + t.slice(1);
}
