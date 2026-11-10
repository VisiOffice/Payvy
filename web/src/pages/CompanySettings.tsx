// Settings > Company: the organization's name, as people see it in the app
// and in sign-in emails. Renaming is org.update (person_only).
import { useEffect, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { HStack } from '@astryxdesign/core/HStack';
import { MetadataList, MetadataListItem } from '@astryxdesign/core/MetadataList';
import { Spinner } from '@astryxdesign/core/Spinner';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { date } from '../money';
import { Page } from '../Page';
import { useSession } from '../session';

interface Organization {
  id: string;
  name: string;
  created_at: string;
}

/** Mirrors org.update's check; the server still decides. */
const NAME_MAX = 100;

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function CompanySettings() {
  const { me, setMe } = useSession();
  const toast = useToast();
  const [org, setOrg] = useState<Organization | null>(null);
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    run<{ organization: Organization }>('org.get', {})
      .then((r) => {
        setOrg(r.result.organization);
        setName(r.result.organization.name);
      })
      .catch((e) => setError(message(e)));
  }, []);

  const trimmed = name.trim();
  const tooLong = [...trimmed].length > NAME_MAX;
  const changed = !!org && trimmed !== org.name;

  async function save() {
    setBusy(true);
    setSaveError(null);
    try {
      const r = await run<{ organization: Organization }>('org.update', { name: trimmed });
      const next = r.result.organization;
      setOrg(next);
      setName(next.name);
      setMe({ ...me, organization: { ...me.organization, name: next.name } });
      toast({ body: `Renamed to ${next.name}` });
    } catch (e) {
      setSaveError(message(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Page title="Company" narrow description="Your company's name, as people see it in Payvy and in sign-in emails.">
      {error && <Banner status="error" title="Couldn't load the company" description={error} />}
      {!org && !error && <Spinner label="Loading" />}
      {org && (
        <Card>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              if (changed && trimmed && !tooLong && !busy) void save();
            }}
          >
            <VStack gap={4}>
              <TextInput
                label="Company name"
                value={name}
                onChange={setName}
                isRequired
                isDisabled={busy}
                description="Bills, vendors and the record stay as they are."
              />
              {!trimmed && <Banner status="warning" title="The name can't be empty" />}
              {tooLong && <Banner status="warning" title={`The name can be at most ${NAME_MAX} characters`} />}
              {saveError && <Banner status="error" title={saveError} />}
              <MetadataList>
                <MetadataListItem label="Created">{date(org.created_at)}</MetadataListItem>
              </MetadataList>
              <HStack gap={2}>
                <Button
                  label="Save"
                  type="submit"
                  variant="primary"
                  isDisabled={busy || !changed || !trimmed || tooLong}
                />
                {changed && (
                  <Button label="Cancel" variant="ghost" isDisabled={busy} onClick={() => setName(org.name)} />
                )}
              </HStack>
            </VStack>
          </form>
        </Card>
      )}
    </Page>
  );
}
