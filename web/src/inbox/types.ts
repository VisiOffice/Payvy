import type { ApprovalTurn } from '../approval';
import type { Workflow } from '../workflow';

export interface Bill {
  id: string;
  vendor_id: string;
  vendor_name: string;
  invoice_number: string;
  invoice_date: string;
  due_date: string | null;
  amount_cents: number;
  memo: string | null;
  state: string;
}

export interface BillItem {
  bill: Bill;
  progress: { status: string; waiting_on?: string[]; by?: string[]; reason?: string } | null;
  /** While it waits for approval: whether you can give it, and who else can. */
  approval?: ApprovalTurn;
  /** look: waiting on someone other than you. */
  action: 'approve' | 'release' | 'look';
}

export interface Payee {
  id: string;
  vendor_id: string;
  recipient_name: string;
  method: string;
  destination: string;
  reason: string | null;
  proposed_by: string;
  created_at: string;
}

export interface PayeeChange {
  is_current_connection: boolean;
  current_is_current_connection: boolean;
  proposal: Payee;
  vendor_name: string;
  current: Payee | null;
}

export interface WorkflowProposal {
  proposal: {
    id: string;
    vendor_id: string | null;
    workflow: Workflow;
    reason: string | null;
    proposed_by: string;
    created_at: string;
  };
  vendor_name: string | null;
  current: Workflow | null;
}

export interface Inbox {
  bills: BillItem[];
  payee_changes: PayeeChange[];
  workflow_proposals: WorkflowProposal[];
  count: number;
}

export const METHOD: Record<string, string> = {
  ach: 'ACH',
  check: 'Check',
  domestic_wire: 'Wire',
  real_time_payment: 'Real-time payment',
};
