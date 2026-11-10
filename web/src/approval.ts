// Whether you can approve the step a bill is on, as the server works it out
// (payvy-domain duties.rs): the step's approvers, one approval per person per
// bill, and the company's rule about who entered the bill. The server still
// decides; this only keeps an Approve button off bills it would refuse.
import type { Names } from './workflow';

export interface ApprovalTurn {
  step_id: string;
  can_approve: boolean;
  refusal?: { code: string; later_step_id?: string; step_id?: string; approvers?: string[] };
  message?: string;
  others: string[];
  anyone: boolean;
}

const REASON: Record<string, string> = {
  not_a_person: 'Only a person can approve',
  not_an_approver: "You aren't an approver on this step",
  already_approved_step: 'You already approved this step',
  already_approved_bill: 'You already approved this bill',
  entered: 'You entered this bill',
  uploaded: "You uploaded this bill's document",
  last_approver: 'A later step needs your approval',
};

function list(names: string[]): string {
  return names.length < 2 ? (names[0] ?? '') : `${names.slice(0, -1).join(', ')} or ${names.at(-1)}`;
}

/** "You entered this bill. Waiting on Maria." */
export function waitingOnOthers(t: ApprovalTurn, name: Names = (a) => a): string {
  const why = REASON[t.refusal?.code ?? ''] ?? "You can't approve this step";
  const others = t.others.map(name);
  const who = t.anyone
    ? others.length
      ? `Any other person can approve it, such as ${list(others)}.`
      : 'Any other person can approve it.'
    : others.length
      ? `Waiting on ${list(others)}.`
      : "Nobody else can approve it: reject it, or change the workflow and enter it again.";
  return `${why}. ${who}`;
}
