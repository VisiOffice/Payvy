// Settings > Approvals: separation of duties. Changing a rule is a person's
// act (approval_rules.set is person_only) and lands in the event log; the
// server applies the rules on every approval (payvy-domain duties.rs).
import { useCallback, useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Card } from '@astryxdesign/core/Card';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Switch } from '@astryxdesign/core/Switch';
import { Text } from '@astryxdesign/core/Text';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { date } from '../money';
import { Page } from '../Page';

interface Rules {
  submitter_cannot_approve: boolean;
  updated_by: string | null;
  updated_at: string | null;
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function ApprovalSettings() {
  const toast = useToast();
  const [rules, setRules] = useState<Rules | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setRules((await run<Rules>('approval_rules.get', {})).result);
    } catch (e) {
      setError(message(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  async function change(on: boolean) {
    try {
      const r = await run<Rules>('approval_rules.set', { submitter_cannot_approve: on });
      setRules(r.result);
      toast({ body: on ? 'Someone else approves what a person enters' : 'Anyone on the step can approve' });
    } catch (e) {
      toast({ body: message(e), type: 'error' });
      await load();
    }
  }

  return (
    <Page
      title="Approvals"
      narrow
      description="Who may approve a bill, on top of the approvers each workflow step names."
    >
      {error && <Banner status="error" title="Couldn't load the approval rules" description={error} />}
      {!rules && !error && <Spinner label="Loading" />}
      {rules && (
        <Card>
          <VStack gap={4}>
            <Switch
              label="The person who enters a bill can't approve it"
              description="Whoever entered a bill or uploaded its document needs someone else to approve it. Bills an agent enters, like those emailed to your email-in address, aren't affected."
              value={rules.submitter_cannot_approve}
              changeAction={change}
            />
            <Text type="supporting">
              Always on: one person gives a bill one approval, so a workflow with two approval steps always has two
              different people sign off.
            </Text>
            {rules.updated_by && (
              <MetadataList>
                <MetadataListItem label="Last changed">
                  {rules.updated_at ? date(rules.updated_at) : '—'} by {rules.updated_by}
                </MetadataListItem>
              </MetadataList>
            )}
          </VStack>
        </Card>
      )}
    </Page>
  );
}
