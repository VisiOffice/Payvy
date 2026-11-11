import { Allowed, useAccessVersion } from '../permissions';
import { useCallback, useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { Page } from '../Page';
import { run } from '../api/client';
import { ReasonDialog } from '../inbox/ReasonDialog';
import { ReleaseDialog } from '../inbox/ReleaseDialog';
import { METHOD, type Bill, type BillItem, type Inbox as InboxData, type PayeeChange, type WorkflowProposal } from '../inbox/types';
import { DueText } from '../Due';
import { date, dueLabel, money } from '../money';
import { describeStep, type Names } from '../workflow';
import { loadPeople } from '../workflows/Builder';
import { waitingOnOthers } from '../approval';

type Open =
  | { kind: 'release'; bill: Bill }
  | { kind: 'reject'; bill: Bill }
  | { kind: 'decline_payee'; change: PayeeChange }
  | { kind: 'decline_workflow'; proposal: WorkflowProposal }
  | null;

export function Inbox() {
  const access = useAccessVersion();
  const [view, setView] = useState('all');
  const [data, setData] = useState<InboxData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<Open>(null);
  const [names, setNames] = useState<Names>(() => (a: string) => a);
  const toast = useToast();

  // Steps name people by name, not by actor id. Without the list, ids will do.
  useEffect(() => {
    loadPeople()
      .then((ps) => setNames(() => (a: string) => ps.find((p) => p.value === a)?.label ?? a))
      .catch(() => {});
  }, []);

  const load = useCallback(() => {
    run<InboxData>('inbox.get', {})
      .then((r) => {
        setData(r.result);
        setError(null);
      })
      .catch((e) => setError(e.message));
  }, []);
  useEffect(() => { setOpen(null); load(); }, [load, access]);

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
    <Page title="Waiting on you" eyebrow="Action center" description="A clear path from invoice to paid. Here’s what needs your attention.">
      {data && data.count > 0 && <>
        <div className="payvy-overview">
          <div><span className="payvy-stat-label">Needs approval</span><strong>{data.bills.filter(b => b.action === 'approve').length}<span>{data.bills.filter(b => b.action === 'approve').length === 1 ? 'bill' : 'bills'}</span></strong></div>
          <div><span className="payvy-stat-label">Ready to pay</span><strong>{money(data.bills.filter(b => b.action === 'release').reduce((s, b) => s + b.bill.amount_cents, 0))}</strong></div>
          <div><span className="payvy-stat-label">Changes to review</span><strong>{data.payee_changes.length + data.workflow_proposals.length}<span>{data.payee_changes.length + data.workflow_proposals.length === 1 ? 'proposal' : 'proposals'}</span></strong></div>
        </div>
        <div className="payvy-viewbar">
          <SegmentedControl label="Queue view" size="sm" value={view} onChange={setView}>
            <SegmentedControlItem value="all" label="Everything" />
            <SegmentedControlItem value="approve" label="Approvals" />
            <SegmentedControlItem value="release" label="Payments" />
            <SegmentedControlItem value="changes" label="Changes" />
          </SegmentedControl>
          <span className="payvy-muted">{data.count} to review</span>
        </div>
      </>}

      {error && <Banner status="error" title="Couldn't load your inbox" description={error} />}
      {!data && !error && <Spinner label="Loading" />}
      {data && data.count === 0 && (
        <EmptyState
          title="Nothing is waiting on you"
          description="Bills that need your approval or release, and payee or workflow changes, show up here."
        />
      )}

      {data && (['approve', 'release', 'look'] as const).map(action => {
        const items = data.bills.filter(b => b.action === action);
        if (!items.length || (view !== 'all' && view !== action)) return null;
        return <section className="payvy-queue-section" key={action} aria-label={action === 'approve' ? 'Bills to approve' : action === 'release' ? 'Bills to pay' : 'Bills waiting on someone else'}>
          <div className="payvy-section-heading">
            <Heading level={2}>{action === 'approve' ? 'Needs approval' : action === 'release' ? 'Ready to pay' : 'Waiting on someone else'}</Heading>
            <span className="payvy-count">{items.length}</span>
            <span className="payvy-section-total">{money(items.reduce((s, i) => s + i.bill.amount_cents, 0))}</span>
          </div>
          <div className="payvy-queue">
            {items.map(item => <BillCard key={item.bill.id} item={item} names={names}
              onApprove={() => act(() => run('bill.approve', { bill_id: item.bill.id }), `Approved ${item.bill.vendor_name} ${item.bill.invoice_number}`)}
              onRelease={() => setOpen({ kind: 'release', bill: item.bill })}
              onReject={() => setOpen({ kind: 'reject', bill: item.bill })} />)}
          </div>
        </section>;
      })}
      {data && data.count > 0 && ((view === 'approve' || view === 'release') && !data.bills.some(b => b.action === view)
        || view === 'changes' && !data.payee_changes.length && !data.workflow_proposals.length) &&
        <EmptyState title="You’re all caught up here" description="Choose another view to continue reviewing." />}

      {data && (view === 'all' || view === 'changes') && data.payee_changes.length > 0 && (
        <VStack gap={3}>
          <HStack gap={3} vAlign="center" wrap="wrap">
            <Heading level={2}>Where vendors are paid</Heading>
            <Text type="supporting">{data.payee_changes.length} proposed change{data.payee_changes.length === 1 ? '' : 's'}</Text>
          </HStack>
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

      {data && (view === 'all' || view === 'changes') && data.workflow_proposals.length > 0 && (
        <VStack gap={3}>
          <Heading level={2}>Workflow changes</Heading>
          {data.workflow_proposals.map((w) => (
            <WorkflowCard
              key={w.proposal.id}
              proposal={w}
              names={names}
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
    </Page>
  );
}

function BillCard({
  item,
  names,
  onApprove,
  onRelease,
  onReject,
}: {
  item: BillItem;
  names: Names;
  onApprove: () => Promise<void>;
  onRelease: () => void;
  onReject: () => void;
}) {
  const b = item.bill;
  return (
    <div className="payvy-queue-row">
      <span className="payvy-vendor-glyph" aria-hidden="true">{b.vendor_name.slice(0, 1).toUpperCase()}</span>
      <div className="payvy-queue-description">
        <Link href={`/bills/${b.id}`} isStandalone>{b.vendor_name}</Link>
        <span className="payvy-muted">{b.invoice_number}{b.memo ? ` · ${b.memo}` : ''}</span>
        {item.progress?.reason && <span className="payvy-muted">{item.progress.reason}</span>}
        {item.approval && !item.approval.can_approve && (
          <span className="payvy-muted">{waitingOnOthers(item.approval, names)}</span>
        )}
      </div>
      <div className="payvy-queue-due">
        <span>{b.due_date ? date(b.due_date) : 'No due date'}</span>
        {dueLabel(b.due_date) && <DueText iso={b.due_date} />}
      </div>
      <strong className="payvy-queue-amount">{money(b.amount_cents)}</strong>
      <div className="payvy-queue-actions">
        <Allowed command="bill.reject"><Button label="Reject" variant="ghost" size="sm" onClick={onReject} /></Allowed>
        {item.action === 'approve' && <Allowed command="bill.approve"><Button label="Approve" variant="primary" size="sm" clickAction={onApprove} /></Allowed>}
        {item.action === 'release' && <Allowed command="bill.release"><Button label="Pay…" variant="primary" size="sm" onClick={onRelease} /></Allowed>}
      </div>
    </div>
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
            {cur ? `${cur.recipient_name} · ${METHOD[cur.method] ?? cur.method} · ${cur.destination}${change.current_is_current_connection ? '' : ' (previous connection)'}` : 'Not set'}
          </MetadataListItem>
          <MetadataListItem label="Proposed by">
            {p.proposed_by} on {date(p.created_at)}
          </MetadataListItem>
          {p.reason && <MetadataListItem label="Why">{p.reason}</MetadataListItem>}
        </MetadataList>
        {!change.is_current_connection && <Banner status="warning" title="This proposal is for a previous bank connection" description="Decline it and propose a payee on the vendor page for the current connection." />}
        <HStack gap={2} hAlign="end">
          <Button label="Decline" variant="ghost" onClick={onDecline} />
          <Button label="Accept" variant="primary" isDisabled={!change.is_current_connection} clickAction={onAccept} />
        </HStack>
      </VStack>
    </Card>
  );
}

function WorkflowCard({
  proposal,
  names,
  onAccept,
  onDecline,
}: {
  proposal: WorkflowProposal;
  names: Names;
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
          <Steps title="Proposed" steps={p.workflow.steps.map((s) => describeStep(s, names))} />
          <Steps
            title="Now"
            steps={proposal.current ? proposal.current.steps.map((s) => describeStep(s, names)) : ['The default workflow']}
          />
        </HStack>
        <Text type="supporting">
          Proposed by {p.proposed_by} on {date(p.created_at)}
          {p.reason ? `: ${p.reason}` : ''}
        </Text>
        <HStack gap={2} hAlign="end">
          <Button
            label="Open in builder"
            variant="ghost"
            href={p.vendor_id ? `/vendors/${p.vendor_id}#workflow` : '/workflows'}
          />
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
