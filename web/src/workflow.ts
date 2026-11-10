// Plain-language descriptions of workflow steps, until the visual builder.
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

export function describeStep(s: Step): string {
  switch (s.type) {
    case 'approve': {
      const above = s.min_amount_cents ? ` for bills of ${money(s.min_amount_cents)} or more` : '';
      return `Approved by ${people(s.approvers, s.require)}${above}`;
    }
    case 'hold':
      return `Wait ${s.days} day${s.days === 1 ? '' : 's'}`;
    case 'wait_for':
      return `Wait until ${s.label ?? s.signal}`;
    case 'pay':
      return s.release === 'auto'
        ? 'Paid automatically'
        : `Payment released by ${people(s.by)}`;
  }
}
