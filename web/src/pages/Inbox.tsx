import { useCallback, useEffect, useState } from 'react';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { ReasonDialog } from '../inbox/ReasonDialog';
import { ReleaseDialog } from '../inbox/ReleaseDialog';
import { METHOD, type Bill, type BillItem, type Inbox as InboxData, type PayeeChange, type WorkflowProposal } from '../inbox/types';
import { date, money } from '../money';
import { describeStep } from '../workflow';

type Open =
  | { kind: 'release'; bill: Bill }
  | { kind: 'reject'; bill: Bill }
  | { kind: 'decline_payee'; change: PayeeChange }
  | { kind: 'decline_workflow'; proposal: WorkflowProposal }
  | null;

export function Inbox() {
  const [data, setData] = useState<InboxData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<Open>(null);
  const toast = useToast();

  const load = useCallback(() => {
    run<InboxData>('inbox.get', {})
      .then((r) => {
        setData(r.result);
        setError(null);
      })
      .catch((e) => setError(e.message));
  }, []);
  useEffect(load, [load]);

  /** Runs an action, says what happened, and reloads the inbox. */
  const act = useCallback(
    async (fn: () => Promise<unknown>, done: string) => {
      try {
        await fn();
        toast({ body: done });
        setOpen(null);
        load();
      } catch (e) {
        toast({ body: e instanceof Error ? e.message : String(e), type: 'error' });
        throw e;
      }
    },
    [toast, load],
  );

  return (
    <VStack gap={6} padding={6} maxWidth={880}>
      <VStack gap={1}>
        <Heading level={1}>Waiting on you</Heading>
        <Text type="supporting">Bills to approve or pay, and changes only a person can accept.</Text>
      </VStack>

      {error && <Banner status="error" title="Couldn't load your inbox" description={error} />}
      {!data && !error && <Spinner label="Loading" />}
      {data && data.count === 0 && (
        <EmptyState
          title="Nothing is waiting on you"
          description="Bills that need your approval or release, and payee or workflow changes, show up here."
        />
      )}

      {data && data.bills.length > 0 && (
        <VStack gap={3}>
          <Heading level={2}>Bills</Heading>
          {data.bills.map((item) => (
            <BillCard
              key={item.bill.id}
              item={item}
              onApprove={() =>
                act(
                  () => run('bill.approve', { bill_id: item.bill.id }),
                  `Approved ${item.bill.vendor_name} ${item.bill.invoice_number}`,
                )
              }
              onRelease={() => setOpen({ kind: 'release', bill: item.bill })}
              onReject={() => setOpen({ kind: 'reject', bill: item.bill })}
            />
          ))}
        </VStack>
      )}

      {data && data.payee_changes.length > 0 && (
        <VStack gap={3}>
          <Heading level={2}>Where vendors are paid</Heading>
          <Banner
            status="warning"
            title="Check each change with the vendor before accepting it"
            description="Call them at a number you already have, not one from the email that asked for the change. Changed bank details are how most AP fraud happens."
          />
          {data.payee_changes.map((c) => (
            <PayeeCard
              key={c.proposal.id}
              change={c}
              onAccept={() =>
                act(
                  () => run('vendor.accept_payee', { proposal_id: c.proposal.id }),
                  `${c.vendor_name} will be paid to ${c.proposal.recipient_name} ${c.proposal.destination}`,
                )
              }
              onDecline={() => setOpen({ kind: 'decline_payee', change: c })}
            />
          ))}
        </VStack>
      )}

      {data && data.workflow_proposals.length > 0 && (
        <VStack gap={3}>
          <Heading level={2}>Workflow changes</Heading>
          {data.workflow_proposals.map((w) => (
            <WorkflowCard
              key={w.proposal.id}
              proposal={w}
              onAccept={() =>
                act(
                  () => run('workflow.accept', { proposal_id: w.proposal.id }),
                  `New bills from ${w.vendor_name ?? 'every vendor without its own'} follow the new workflow`,
                )
              }
              onDecline={() => setOpen({ kind: 'decline_workflow', proposal: w })}
            />
          ))}
        </VStack>
      )}

      {open?.kind === 'release' && (
        <ReleaseDialog
          bill={open.bill}
          onClose={() => setOpen(null)}
          onReleased={(message) => {
            toast({ body: message });
            setOpen(null);
            load();
          }}
        />
      )}
      {open?.kind === 'reject' && (
        <ReasonDialog
          title={`Reject ${open.bill.vendor_name} ${open.bill.invoice_number}?`}
          subtitle={`${money(open.bill.amount_cents)}. It won't be paid.`}
          actionLabel="Reject"
          isRequired
          onClose={() => setOpen(null)}
          onSubmit={(reason) =>
            act(() => run('bill.reject', { bill_id: open.bill.id, reason }), 'Rejected')
          }
        />
      )}
      {open?.kind === 'decline_payee' && (
        <ReasonDialog
          title={`Decline the change for ${open.change.vendor_name}?`}
          subtitle="Bills keep being paid where they are now."
          actionLabel="Decline"
          isRequired={false}
          onClose={() => setOpen(null)}
          onSubmit={(reason) =>
            act(
              () =>
                run('vendor.decline_payee', {
                  proposal_id: open.change.proposal.id,
                  ...(reason ? { reason } : {}),
                }),
              'Declined',
            )
          }
        />
      )}
      {open?.kind === 'decline_workflow' && (
        <ReasonDialog
          title="Decline this workflow?"
          subtitle="Bills keep following the current one."
          actionLabel="Decline"
          isRequired={false}
          onClose={() => setOpen(null)}
          onSubmit={(reason) =>
            act(
              () =>
                run('workflow.decline', {
                  proposal_id: open.proposal.proposal.id,
                  ...(reason ? { reason } : {}),
                }),
              'Declined',
            )
          }
        />
      )}
    </VStack>
  );
}

function BillCard({
  item,
  onApprove,
  onRelease,
  onReject,
}: {
  item: BillItem;
  onApprove: () => Promise<void>;
  onRelease: () => void;
  onReject: () => void;
}) {
  const b = item.bill;
  return (
    <Card>
      <HStack gap={4} vAlign="center" hAlign="between" wrap="wrap">
        <VStack gap={1}>
          <HStack gap={2} vAlign="center">
            <Text weight="semibold">{b.vendor_name}</Text>
            <Badge
              label={item.action === 'approve' ? 'Approve' : item.action === 'release' ? 'Pay' : 'Look'}
              variant={item.action === 'release' ? 'info' : 'warning'}
            />
          </HStack>
          <Text type="supporting">
            Invoice {b.invoice_number} · {date(b.invoice_date)}
            {b.due_date ? ` · due ${date(b.due_date)}` : ''}
          </Text>
          {b.memo && <Text type="supporting">{b.memo}</Text>}
        </VStack>
        <HStack gap={3} vAlign="center">
          <Text weight="semibold" hasTabularNumbers>
            {money(b.amount_cents)}
          </Text>
          <Button label="Reject" variant="ghost" onClick={onReject} />
          {item.action === 'approve' && <Button label="Approve" variant="primary" clickAction={onApprove} />}
          {item.action === 'release' && <Button label="Pay…" variant="primary" onClick={onRelease} />}
        </HStack>
      </HStack>
    </Card>
  );
}

function PayeeCard({
  change,
  onAccept,
  onDecline,
}: {
  change: PayeeChange;
  onAccept: () => Promise<void>;
  onDecline: () => void;
}) {
  const p = change.proposal;
  const cur = change.current;
  return (
    <Card>
      <VStack gap={3}>
        <Text weight="semibold">{change.vendor_name}</Text>
        <MetadataList>
          <MetadataListItem label="New">
            {p.recipient_name} · {METHOD[p.method] ?? p.method} · {p.destination}
          </MetadataListItem>
          <MetadataListItem label="Now">
            {cur ? `${cur.recipient_name} · ${METHOD[cur.method] ?? cur.method} · ${cur.destination}` : 'Not set'}
          </MetadataListItem>
          <MetadataListItem label="Proposed by">
            {p.proposed_by} on {date(p.created_at)}
          </MetadataListItem>
          {p.reason && <MetadataListItem label="Why">{p.reason}</MetadataListItem>}
        </MetadataList>
        <HStack gap={2} hAlign="end">
          <Button label="Decline" variant="ghost" onClick={onDecline} />
          <Button label="Accept" variant="primary" clickAction={onAccept} />
        </HStack>
      </VStack>
    </Card>
  );
}

function WorkflowCard({
  proposal,
  onAccept,
  onDecline,
}: {
  proposal: WorkflowProposal;
  onAccept: () => Promise<void>;
  onDecline: () => void;
}) {
  const p = proposal.proposal;
  return (
    <Card>
      <VStack gap={3}>
        <Text weight="semibold">
          {proposal.vendor_name ?? 'Default for vendors without their own'}
        </Text>
        <HStack gap={6} wrap="wrap" vAlign="start">
          <Steps title="Proposed" steps={p.workflow.steps.map(describeStep)} />
          <Steps
            title="Now"
            steps={proposal.current ? proposal.current.steps.map(describeStep) : ['The default workflow']}
          />
        </HStack>
        <Text type="supporting">
          Proposed by {p.proposed_by} on {date(p.created_at)}
          {p.reason ? `: ${p.reason}` : ''}
        </Text>
        <HStack gap={2} hAlign="end">
          <Button label="Decline" variant="ghost" onClick={onDecline} />
          <Button label="Accept" variant="primary" clickAction={onAccept} />
        </HStack>
      </VStack>
    </Card>
  );
}

function Steps({ title, steps }: { title: string; steps: string[] }) {
  return (
    <VStack gap={1}>
      <Text type="label">{title}</Text>
      <ol style={{ margin: 0, paddingInlineStart: 20 }}>
        {steps.map((s, i) => (
          <li key={i}>
            <Text>{s}</Text>
          </li>
        ))}
      </ol>
    </VStack>
  );
}
