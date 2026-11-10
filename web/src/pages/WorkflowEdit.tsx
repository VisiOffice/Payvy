// Builds a workflow for one vendor, or the organization default. Saving
// proposes it and, since the person saving can accept it, accepts it at once;
// "Propose" leaves it for someone else to accept. Either way it goes through
// workflow.propose and workflow.accept like any agent's proposal, so the
// record shows who changed what.
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useParams } from 'react-router';
import { Banner } from '@astryxdesign/core/Banner';
import { BreadcrumbItem, Breadcrumbs } from '@astryxdesign/core/Breadcrumbs';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { Divider } from '@astryxdesign/core/Divider';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import type { WorkflowProposeInput } from '../api/commands.gen';
import { ReasonDialog } from '../inbox/ReasonDialog';
import { date } from '../money';
import { clean, newStep, problems, same, type Current, type Proposal, type Step, type StepType } from '../workflow';
import { Chart } from '../workflows/Chart';
import { StepEditor, type PersonOption } from '../workflows/StepEditor';

interface Loaded {
  current: Current;
  vendorName: string | null;
  people: PersonOption[];
}

export function WorkflowEdit() {
  const { key = 'default' } = useParams();
  const vendorId = key === 'default' ? undefined : key;
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [steps, setSteps] = useState<Step[]>([]);
  const [selected, setSelected] = useState<string | undefined>();
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [declining, setDeclining] = useState<Proposal | null>(null);
  const toast = useToast();

  const load = useCallback(async () => {
    try {
      const [w, p, v] = await Promise.all([
        run<Current>('workflow.get', vendorId ? { vendor_id: vendorId } : {}),
        run<{ people: { actor: string; name: string }[] }>('person.list', {}),
        vendorId
          ? run<{ vendor: { name: string } }>('vendor.get', { vendor_id: vendorId })
          : Promise.resolve(null),
      ]);
      setLoaded({
        current: w.result,
        vendorName: v?.result.vendor.name ?? null,
        people: p.result.people.map((x) => ({ value: x.actor, label: x.name })),
      });
      setSteps(w.result.workflow.steps);
      setSelected(undefined);
      setReason('');
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [vendorId]);
  useEffect(() => {
    void load();
  }, [load]);

  const issues = useMemo(() => problems(steps), [steps]);
  const changed = loaded ? !same(steps, loaded.current.workflow.steps) : false;
  const ok = Object.keys(issues).length === 0;
  const index = steps.findIndex((s) => s.id === selected);
  const step = index >= 0 ? steps[index] : undefined;
  const whose = loaded?.vendorName ?? 'vendors without their own workflow';
  const names = useMemo(() => {
    const byActor = new Map(loaded?.people.map((p) => [p.value, p.label]));
    return (actor: string) => byActor.get(actor) ?? actor;
  }, [loaded]);

  function insert(at: number, type: StepType) {
    const s = newStep(type, steps);
    setSteps([...steps.slice(0, at), s, ...steps.slice(at)]);
    setSelected(s.id);
  }
  function update(next: Step) {
    setSteps(steps.map((s) => (s.id === next.id ? next : s)));
  }
  function move(by: -1 | 1) {
    const to = index + by;
    // The pay step stays last.
    if (to < 0 || to >= steps.length - 1) return;
    const next = [...steps];
    [next[index], next[to]] = [next[to]!, next[index]!];
    setSteps(next);
  }
  function remove() {
    setSteps(steps.filter((s) => s.id !== selected));
    setSelected(undefined);
  }

  /** Proposes the steps on screen; with `accept`, accepts them too. */
  async function save(accept: boolean) {
    setBusy(true);
    try {
      const input: WorkflowProposeInput = {
        ...(vendorId ? { vendor_id: vendorId } : {}),
        // The generated type spells steps as a tuple; they are checked on the server.
        workflow: { steps: clean(steps) as WorkflowProposeInput['workflow']['steps'] },
        ...(reason.trim() ? { reason: reason.trim() } : {}),
      };
      const proposed = await run<{ proposal_id: string }>('workflow.propose', input);
      if (accept) {
        await run('workflow.accept', { proposal_id: proposed.result.proposal_id });
        toast({ body: `New bills from ${whose} follow this workflow. Bills already here keep theirs.` });
      } else {
        toast({ body: 'Proposed. Someone accepts it from Waiting on you.' });
      }
      await load();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    } finally {
      setBusy(false);
    }
  }

  async function accept(p: Proposal) {
    try {
      await run('workflow.accept', { proposal_id: p.proposal_id });
      toast({ body: `New bills from ${whose} follow the accepted workflow.` });
      await load();
    } catch (e) {
      toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
    }
  }

  if (error) return <Banner status="error" title="Couldn't load this workflow" description={error} />;
  if (!loaded) return <Spinner label="Loading" />;
  const { current } = loaded;

  return (
    <VStack gap={6} padding={6}>
      <VStack gap={2}>
        <Breadcrumbs>
          <BreadcrumbItem href="/workflows">Workflows</BreadcrumbItem>
          <BreadcrumbItem isCurrent>{loaded.vendorName ?? 'Default'}</BreadcrumbItem>
        </Breadcrumbs>
        <Heading level={1}>{loaded.vendorName ? `${loaded.vendorName} workflow` : 'Default workflow'}</Heading>
        <Text type="supporting">{sourceNote(current, loaded.vendorName)}</Text>
      </VStack>

      <Card>
        <VStack gap={5}>
          <VStack gap={1}>
            <Heading level={2}>Steps</Heading>
            <Text type="supporting">
              Choose a step to change it, or + to add one there. Steps run in order; the last one pays.
            </Text>
          </VStack>
          <Chart
            steps={steps}
            selected={selected}
            problems={issues}
            onSelect={setSelected}
            onInsert={insert}
            label="Workflow being edited"
            names={names}
          />
          {step && (
            <>
              <Divider />
              <div style={{ maxWidth: 520 }}>
                <StepEditor
                  key={step.id}
                  step={step}
                  people={loaded.people}
                  problem={issues[step.id]}
                  canMoveEarlier={index > 0}
                  canMoveLater={index < steps.length - 2}
                  onChange={update}
                  onMove={move}
                  onRemove={remove}
                />
              </div>
            </>
          )}
          {changed && (
            <>
              <Divider />
              <VStack gap={3}>
                <TextInput
                  width={520}
                  label="Why the change"
                  isOptional
                  description="Kept with the change, for whoever reads the history."
                  value={reason}
                  onChange={setReason}
                />
                <HStack gap={2} hAlign="end" wrap="wrap">
                  <Button
                    label="Discard changes"
                    variant="ghost"
                    isDisabled={busy}
                    onClick={() => {
                      setSteps(current.workflow.steps);
                      setSelected(undefined);
                    }}
                  />
                  <Button
                    label="Propose for someone else to accept"
                    isDisabled={!ok || busy}
                    clickAction={() => save(false)}
                  />
                  <Button label="Save" variant="primary" isDisabled={!ok || busy} clickAction={() => save(true)} />
                </HStack>
                {!ok && (
                  <Text type="supporting">Fix the steps marked in red to save.</Text>
                )}
              </VStack>
            </>
          )}
        </VStack>
      </Card>

      {current.proposals.length > 0 && (
        <VStack gap={3}>
          <Heading level={2}>Proposed changes</Heading>
          {current.proposals.map((p) => (
            <Card key={p.proposal_id}>
              <VStack gap={3}>
                <Chart steps={p.workflow.steps} label={`Proposed by ${p.proposed_by}`} names={names} />
                <Text type="supporting">
                  Proposed by {p.proposed_by} on {date(p.proposed_at)}
                  {p.reason ? `: ${p.reason}` : ''}
                </Text>
                <HStack gap={2} hAlign="end" wrap="wrap">
                  <Button
                    label="Edit from this"
                    variant="ghost"
                    onClick={() => {
                      setSteps(p.workflow.steps);
                      setSelected(undefined);
                    }}
                  />
                  <Button label="Decline" variant="ghost" onClick={() => setDeclining(p)} />
                  <Button label="Accept" variant="primary" clickAction={() => accept(p)} />
                </HStack>
              </VStack>
            </Card>
          ))}
        </VStack>
      )}

      {declining && (
        <ReasonDialog
          title="Decline this workflow?"
          subtitle={`Proposed by ${declining.proposed_by}`}
          actionLabel="Decline"
          isRequired={false}
          onClose={() => setDeclining(null)}
          onSubmit={async (why) => {
            await run('workflow.decline', {
              proposal_id: declining.proposal_id,
              ...(why ? { reason: why } : {}),
            });
            setDeclining(null);
            toast({ body: 'Declined' });
            await load();
          }}
        />
      )}
    </VStack>
  );
}

function sourceNote(c: Current, vendor: string | null): string {
  const by = c.accepted_by ? `, accepted by ${c.accepted_by}` : '';
  switch (c.source) {
    case 'vendor':
      return `New bills from ${vendor} follow this${by}.`;
    case 'organization':
      return vendor
        ? `${vendor} follows the default workflow. Saving here gives it its own.`
        : `Every vendor without its own workflow follows this${by}.`;
    case 'builtin':
      return vendor
        ? `${vendor} follows Payvy's starting workflow: a person releases each bill. Saving here gives it its own.`
        : "Payvy's starting workflow: a person releases each bill. Saving makes this your default.";
  }
}
