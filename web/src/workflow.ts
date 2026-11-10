// Workflow steps as the server defines them (payvy-domain workflow.rs), read aloud and edited.
import { money } from './money';

export interface Step {
  id: string;
  type: 'approve' | 'hold' | 'wait_for' | 'pay';
  approvers?: string[];
  require?: 'any' | 'all';
  min_amount_cents?: number;
  days?: number;
  signal?: string;
  label?: string;
  release?: 'manual' | 'auto';
  by?: string[];
}

export interface Workflow {
  steps: Step[];
}

function people(names: string[] | undefined, require: 'any' | 'all' = 'any'): string {
  if (!names || names.length === 0) return 'anyone';
  if (names.length === 1) return names[0]!;
  const list = names.slice(0, -1).join(', ') + ` ${require === 'all' ? 'and' : 'or'} ${names.at(-1)}`;
  return require === 'all' ? `all of ${list}` : list;
}

/** Shows actor names as people's names where they're known. */
export type Names = (actor: string) => string;

export function describeStep(s: Step, name: Names = (a) => a): string {
  switch (s.type) {
    case 'approve': {
      const above = s.min_amount_cents ? ` for bills of ${money(s.min_amount_cents)} or more` : '';
      return `Approved by ${people(s.approvers?.map(name), s.require)}${above}`;
    }
    case 'hold':
      return `Wait ${s.days} day${s.days === 1 ? '' : 's'}`;
    case 'wait_for':
      return `Wait until ${s.label?.trim() || s.signal || 'something happens'}`;
    case 'pay':
      return s.release === 'auto'
        ? 'Paid automatically'
        : `Payment released by ${people(s.by?.map(name))}`;
  }
}

export type StepType = Step['type'];

/** What each kind of step is called in the builder. */
export const KIND: Record<StepType, string> = {
  approve: 'Approval',
  hold: 'Hold',
  wait_for: 'Wait for',
  pay: 'Pay',
};

const ID_BASE: Record<StepType, string> = { approve: 'approve', hold: 'hold', wait_for: 'wait', pay: 'pay' };

/** A new step of this kind, with an id no other step uses. */
export function newStep(type: StepType, steps: Step[]): Step {
  const used = new Set(steps.map((s) => s.id));
  const base = ID_BASE[type];
  let id = base;
  for (let n = 2; used.has(id); n++) id = `${base}-${n}`;
  switch (type) {
    case 'approve':
      return { id, type, approvers: [], require: 'any' };
    case 'hold':
      return { id, type, days: 3 };
    case 'wait_for':
      return { id, type, signal: '', label: '' };
    case 'pay':
      return { id, type, release: 'manual', by: [] };
  }
}

/** A label as a signal key: "the customer's payment" → "customer_payment". */
export function signalKey(label: string): string {
  return label
    .toLowerCase()
    .replace(/^the\s+/, '')
    .replace(/'s\b/g, '')
    .replace(/[^a-z0-9]+/g, '_')
    .replace(/^_+|_+$/g, '')
    .slice(0, 60);
}

/**
 * What's wrong with each step, by step id, in the words a person would fix it
 * with. Mirrors `Workflow::validate` on the server, which still decides.
 */
export function problems(steps: Step[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const s of steps) {
    switch (s.type) {
      case 'approve':
        if (s.require === 'all' && !s.approvers?.length) out[s.id] = 'Choose who must all approve';
        else if (s.min_amount_cents !== undefined && !(s.min_amount_cents >= 0)) out[s.id] = 'Enter the amount like 5,000.00';
        break;
      case 'hold':
        if (!Number.isInteger(s.days) || s.days! < 1 || s.days! > 365) out[s.id] = 'A hold is 1 to 365 days';
        break;
      case 'wait_for':
        if (!s.label?.trim() && !s.signal) out[s.id] = 'Say what to wait for';
        else if (!/^[a-z0-9_]{1,60}$/.test(s.signal ?? ''))
          out[s.id] = 'The signal is 1–60 lowercase letters, digits or _';
        break;
      case 'pay':
        break;
    }
  }
  const twice = needsAPersonTwice(steps);
  if (twice && !out[twice]) out[twice] = 'One person gives a bill one approval, and an earlier step already needs these people. Choose someone else.';
  return out;
}

/**
 * The first approval step whose named approvers can't be someone other than
 * the people earlier steps need: one person gives a bill one approval. Steps
 * anyone may approve never are. Mirrors `duties::needs_a_person_twice`.
 */
export function needsAPersonTwice(steps: Step[]): string | null {
  const slots: { step: string; who: string[] }[] = [];
  for (const s of steps) {
    if (s.type !== 'approve' || !s.approvers?.length) continue;
    if (s.require === 'all') for (const a of s.approvers) slots.push({ step: s.id, who: [a] });
    else slots.push({ step: s.id, who: s.approvers });
  }
  const owner = new Map<string, number>();
  const assign = (i: number, seen: Set<string>): boolean => {
    for (const p of slots[i]!.who) {
      if (seen.has(p)) continue;
      seen.add(p);
      const j = owner.get(p);
      if (j === undefined || assign(j, seen)) {
        owner.set(p, i);
        return true;
      }
    }
    return false;
  };
  for (let i = 0; i < slots.length; i++) if (!assign(i, new Set())) return slots[i]!.step;
  return null;
}

/** The workflow as the server stores it: no empty optional fields. */
export function clean(steps: Step[]): Step[] {
  return steps.map((s) => {
    switch (s.type) {
      case 'approve':
        return {
          id: s.id,
          type: s.type,
          approvers: s.approvers ?? [],
          // With one approver, "any" and "all" are the same thing.
          require: (s.approvers?.length ?? 0) > 1 ? (s.require ?? 'any') : 'any',
          ...(s.min_amount_cents ? { min_amount_cents: s.min_amount_cents } : {}),
        };
      case 'hold':
        return { id: s.id, type: s.type, days: s.days };
      case 'wait_for':
        return { id: s.id, type: s.type, signal: s.signal, ...(s.label?.trim() ? { label: s.label.trim() } : {}) };
      case 'pay':
        return s.release === 'auto'
          ? { id: s.id, type: s.type, release: 'auto', by: [] }
          : { id: s.id, type: s.type, release: 'manual', by: s.by ?? [] };
    }
  });
}

/** Whether two workflows would behave the same. */
export function same(a: Step[], b: Step[]): boolean {
  return JSON.stringify(clean(a)) === JSON.stringify(clean(b));
}

/** What `workflow.get` returns. */
export interface WorkflowView {
  steps: Step[];
  describe: string[];
  mermaid: string;
}

export interface Proposal {
  proposal_id: string;
  vendor_id: string | null;
  status: 'proposed' | 'active' | 'superseded' | 'declined';
  reason: string | null;
  proposed_by: string;
  proposed_at: string;
  workflow: WorkflowView;
}

export interface Current {
  source: 'vendor' | 'organization' | 'builtin';
  workflow_id: string | null;
  accepted_by: string | null;
  workflow: WorkflowView;
  proposals: Proposal[];
}
