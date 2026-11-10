// The path a bill takes, drawn from the same steps the server evaluates: one
// node per step, left to right, ending in the pay step. Workflows are a single
// line (the server has no branches), so this is laid out with CSS rather than
// a graph library. Editable charts select a step on click and insert one at
// each "+"; read-only charts just draw.
import { DropdownMenu } from '@astryxdesign/core/DropdownMenu';
import { describeStep, KIND, type Names, type Step, type StepType } from '../workflow';

const INSERTABLE: { type: StepType; description: string }[] = [
  { type: 'approve', description: 'People sign off before it moves on' },
  { type: 'wait_for', description: 'Until something outside Payvy happens' },
  { type: 'hold', description: 'A number of days' },
];

export function Chart({
  steps,
  selected,
  problems = {},
  onSelect,
  onInsert,
  label,
  names,
}: {
  steps: Step[];
  /** People's names for the actors in approvals and releases. */
  names?: Names;
  selected?: string;
  problems?: Record<string, string>;
  /** Without it the chart is read-only. */
  onSelect?: (id: string) => void;
  onInsert?: (at: number, type: StepType) => void;
  label: string;
}) {
  const editable = !!onSelect;
  return (
    <ol className="payvy-wf" aria-label={label}>
      <li className="payvy-wf-start">Bill received</li>
      {steps.map((s, i) => (
        <li key={s.id} className="payvy-wf-item">
          <span className="payvy-wf-link">
            {onInsert && (
              <DropdownMenu
                hasChevron={false}
                menuWidth={280}
                renderTrigger={(props) => (
                  <button
                    {...props}
                    type="button"
                    className="payvy-wf-add"
                    aria-label={`Add a step before ${describeStep(s, names)}`}
                  >
                    +
                  </button>
                )}
                items={INSERTABLE.map((k) => ({
                  label: KIND[k.type],
                  description: k.description,
                  onClick: () => onInsert(i, k.type),
                }))}
              />
            )}
          </span>
          <Node
            step={s}
            isSelected={selected === s.id}
            problem={problems[s.id]}
            names={names}
            onClick={editable ? () => onSelect(s.id) : undefined}
          />
        </li>
      ))}
    </ol>
  );
}

function Node({
  step,
  isSelected,
  problem,
  names,
  onClick,
}: {
  step: Step;
  isSelected: boolean;
  problem?: string;
  names?: Names;
  onClick?: () => void;
}) {
  const className = [
    'payvy-wf-node',
    `payvy-wf-${step.type}`,
    isSelected ? 'payvy-wf-on' : '',
    problem ? 'payvy-wf-bad' : '',
  ]
    .filter(Boolean)
    .join(' ');
  const body = (
    <>
      <span className="payvy-wf-kind">{KIND[step.type]}</span>
      <span className="payvy-wf-what">{describeStep(step, names)}</span>
      {problem && <span className="payvy-wf-problem">{problem}</span>}
    </>
  );
  return onClick ? (
    <button type="button" className={className} onClick={onClick} aria-pressed={isSelected}>
      {body}
    </button>
  ) : (
    <div className={className}>{body}</div>
  );
}
