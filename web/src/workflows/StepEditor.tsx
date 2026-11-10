// Edits one step of a workflow. Mount it with key={step.id} so its local text
// (an amount being typed) starts over for each step.
import { useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { MultiSelector } from '@astryxdesign/core/MultiSelector';
import { NumberInput } from '@astryxdesign/core/NumberInput';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VStack } from '@astryxdesign/core/VStack';
import { cents, dollars } from '../money';
import { KIND, signalKey, type Step } from '../workflow';

export interface PersonOption {
  value: string;
  label: string;
}

export function StepEditor({
  step,
  people,
  problem,
  canMoveEarlier,
  canMoveLater,
  onChange,
  onMove,
  onRemove,
}: {
  step: Step;
  people: PersonOption[];
  problem?: string;
  canMoveEarlier: boolean;
  canMoveLater: boolean;
  onChange: (step: Step) => void;
  onMove: (by: -1 | 1) => void;
  onRemove: () => void;
}) {
  const set = (patch: Partial<Step>) => onChange({ ...step, ...patch });
  // People who were chosen but have since left still show, so they can be removed.
  const options = (chosen: string[] | undefined) => [
    ...people,
    ...(chosen ?? [])
      .filter((a) => !people.some((p) => p.value === a))
      .map((a) => ({ value: a, label: `${a} (no longer here)` })),
  ];
  const isPay = step.type === 'pay';

  return (
    <VStack gap={4}>
      <HStack gap={2} hAlign="between" vAlign="center">
        <Heading level={3}>{KIND[step.type]}</Heading>
        {!isPay && (
          <HStack gap={1}>
            <Button
              label="Earlier"
              size="sm"
              variant="ghost"
              isDisabled={!canMoveEarlier}
              onClick={() => onMove(-1)}
            />
            <Button
              label="Later"
              size="sm"
              variant="ghost"
              isDisabled={!canMoveLater}
              onClick={() => onMove(1)}
            />
            <Button label="Remove" size="sm" variant="ghost" onClick={onRemove} />
          </HStack>
        )}
      </HStack>

      {step.type === 'approve' && (
        <ApproveFields step={step} options={options(step.approvers)} set={set} problem={problem} />
      )}

      {step.type === 'hold' && (
        <NumberInput
          label="Days to wait"
          description="Counted from when the step before it finished."
          value={step.days ?? null}
          min={1}
          max={365}
          step={1}
          units="days"
          onChange={(v) => set({ days: Number.isFinite(v) ? Math.round(v) : undefined })}
          status={problem ? { type: 'error', message: problem } : undefined}
        />
      )}

      {step.type === 'wait_for' && (
        <VStack gap={3}>
          <TextInput
            label="Wait until"
            description="What has to happen, as people will read it: “the customer's payment”."
            value={step.label ?? ''}
            onChange={(label) =>
              set({
                label,
                // Follow the label until someone sets the signal themselves.
                signal: !step.signal || step.signal === signalKey(step.label ?? '') ? signalKey(label) : step.signal,
              })
            }
          />
          <TextInput
            label="Signal"
            description="An agent or another system sends this with bill.signal when it happens."
            value={step.signal ?? ''}
            onChange={(signal) => set({ signal: signal.toLowerCase() })}
            status={problem ? { type: 'error', message: problem } : undefined}
          />
        </VStack>
      )}

      {isPay && (
        <VStack gap={3}>
          <Text type="supporting">Every workflow ends here. Payvy then tells your bank to pay the vendor.</Text>
          <SegmentedControl
            label="Payment"
            value={step.release ?? 'manual'}
            onChange={(v) => set({ release: v as 'manual' | 'auto' })}
          >
            <SegmentedControlItem value="manual" label="A person releases it" />
            <SegmentedControlItem value="auto" label="Automatically" />
          </SegmentedControl>
          {step.release === 'auto' ? (
            <Banner
              status="warning"
              title="Bills are paid as soon as they get here"
              description="No one releases them: Payvy pays through Mercury to the vendor's accepted payment details within a minute. Put an approval before this step unless every bill from this vendor should be paid unseen. If a payment fails, the bill waits for a person and isn't retried."
            />
          ) : (
            <MultiSelector
              label="Released by"
              description="Leave empty for anyone in your organization."
              placeholder="Anyone"
              triggerDisplay="badges"
              options={options(step.by)}
              value={step.by ?? []}
              onChange={(by) => set({ by })}
            />
          )}
        </VStack>
      )}
    </VStack>
  );
}

function ApproveFields({
  step,
  options,
  set,
  problem,
}: {
  step: Step;
  options: PersonOption[];
  set: (patch: Partial<Step>) => void;
  problem?: string;
}) {
  const [amount, setAmount] = useState(step.min_amount_cents ? dollars(step.min_amount_cents) : '');
  const approvers = step.approvers ?? [];
  return (
    <VStack gap={3}>
      <MultiSelector
        label="Approved by"
        description="Leave empty for anyone in your organization."
        placeholder="Anyone"
        triggerDisplay="badges"
        options={options}
        value={approvers}
        onChange={(next) => set({ approvers: next, require: next.length > 1 ? (step.require ?? 'any') : 'any' })}
        status={problem && approvers.length === 0 ? { type: 'error', message: problem } : undefined}
      />
      {approvers.length > 1 && (
        <SegmentedControl
          label="Needs"
          value={step.require ?? 'any'}
          onChange={(v) => set({ require: v as 'any' | 'all' })}
        >
          <SegmentedControlItem value="any" label="Any one of them" />
          <SegmentedControlItem value="all" label="All of them" />
        </SegmentedControl>
      )}
      <TextInput
        label="Only for bills of at least"
        description="Smaller bills skip this approval. Leave empty to approve every bill."
        placeholder="5,000.00"
        value={amount}
        onChange={(v) => {
          setAmount(v);
          // NaN keeps the step invalid until the amount reads as one.
          set({ min_amount_cents: v.trim() ? (cents(v) ?? Number.NaN) : undefined });
        }}
        status={amount.trim() && cents(amount) === null ? { type: 'error', message: 'Like 5,000.00' } : undefined}
      />
    </VStack>
  );
}
