// Settings > People: who can sign in to this company's Payvy. Adding and
// removing are person.add and person.remove (both person_only). There are no
// roles: everyone who signs in has every right a person has.
import { useCallback, useEffect, useState } from 'react';
import { AlertDialog } from '@astryxdesign/core/AlertDialog';
import { Badge } from '@astryxdesign/core/Badge';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Card } from '@astryxdesign/core/Card';
import { Dialog, DialogHeader } from '@astryxdesign/core/Dialog';
import { HStack } from '@astryxdesign/core/HStack';
import { List, ListItem } from '@astryxdesign/core/List';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { run } from '../api/client';
import { date } from '../money';
import { Page } from '../Page';
import { useSession } from '../session';

/** A row of person.list. */
interface Person {
  id: string;
  actor: string;
  email: string;
  name: string;
  created_at: string;
  created_by: string;
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function PeopleSettings() {
  const { me } = useSession();
  const toast = useToast();
  const [people, setPeople] = useState<Person[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [added, setAdded] = useState<Person | null>(null);
  const [removing, setRemoving] = useState<Person | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      setPeople((await run<{ people: Person[] }>('person.list', {})).result.people);
      setError(null);
    } catch (e) {
      setError(message(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  async function remove() {
    if (!removing) return;
    setBusy(true);
    try {
      await run('person.remove', { actor: removing.actor });
      toast({ body: `${removing.name} can no longer sign in` });
      if (added?.actor === removing.actor) setAdded(null);
      setRemoving(null);
      await load();
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally {
      setBusy(false);
    }
  }

  const onlyOne = people?.length === 1;
  return (
    <Page
      title="People"
      narrow
      description="Everyone who can sign in to this company's Payvy. Each person can approve bills, release payments, and accept payees and workflows."
      actions={people && <Button label="Add person" variant="primary" onClick={() => setAdding(true)} />}
    >
      {error && <Banner status="error" title="Couldn't load people" description={error} />}
      {added && (
        <Banner
          status="success"
          title={`${added.name} can sign in now`}
          description={`Payvy doesn't email them. Tell them to open ${window.location.origin} and enter ${added.email}: they get a sign-in link by email, good once for 15 minutes.`}
          onDismiss={() => setAdded(null)}
        />
      )}
      {!people && !error && <Spinner label="Loading people" />}
      {people && (
        <Card>
          <List hasDividers edge="bleed">
            {people.map((p) => {
              const you = p.actor === me.person.actor;
              return (
                <ListItem
                  key={p.id}
                  itemKey={p.id}
                  label={
                    <HStack gap={2} vAlign="center" wrap="wrap">
                      <Text weight="semibold">{p.name}</Text>
                      {you && <Badge label="You" variant="info" />}
                    </HStack>
                  }
                  description={
                    <HStack gap={3} wrap="wrap">
                      <span className="payvy-wrap">{p.email}</span>
                      <span>
                        Added {date(p.created_at)} by {p.created_by}
                        {p.actor !== p.email.split('@')[0]!.split('+')[0] ? ` · recorded as ${p.actor}` : ''}
                      </span>
                    </HStack>
                  }
                  endContent={
                    !you &&
                    !onlyOne && (
                      <Button
                        label="Remove"
                        aria-label={`Remove ${p.name}`}
                        variant="ghost"
                        size="sm"
                        onClick={() => setRemoving(p)}
                      />
                    )
                  }
                />
              );
            })}
          </List>
        </Card>
      )}
      {people && (
        <Text type="supporting">
          {onlyOne
            ? "You're the only person here. Add someone before you could ever need to be removed."
            : "You can't remove yourself; someone else here can. Removing a person signs them out everywhere, and what they did stays on the record."}
        </Text>
      )}
      {adding && (
        <AddPersonDialog
          onClose={() => setAdding(false)}
          onAdded={async (p) => {
            setAdding(false);
            setAdded(p);
            await load();
          }}
        />
      )}
      <AlertDialog
        isOpen={removing !== null}
        onOpenChange={(open) => !open && !busy && setRemoving(null)}
        title={`Remove ${removing?.name ?? 'this person'}?`}
        description={`${removing?.name ?? 'They'} (${removing?.email ?? ''}) can no longer sign in, and is signed out everywhere at once. What they approved, released or entered stays on the record. You can add them again later.`}
        actionLabel="Remove"
        isActionLoading={busy}
        onAction={remove}
      />
    </Page>
  );
}

function AddPersonDialog({ onClose, onAdded }: { onClose: () => void; onAdded: (p: Person) => void | Promise<void> }) {
  const [name, setName] = useState('');
  const [email, setEmail] = useState('');
  const [actor, setActor] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const emailOk = /^[^\s@]+@[^\s@]+$/.test(email.trim());
  async function add() {
    setBusy(true);
    setError(null);
    try {
      const r = await run<{ person: Person }>('person.add', {
        name: name.trim(),
        email: email.trim(),
        ...(actor.trim() ? { actor: actor.trim() } : {}),
      });
      await onAdded(r.result.person);
    } catch (e) {
      setError(message(e));
      setBusy(false);
    }
  }
  const close = () => {
    if (!busy) onClose();
  };
  return (
    <Dialog isOpen onOpenChange={(o) => !o && close()} width={480} purpose="form">
      <DialogHeader title="Add person" onOpenChange={(o) => !o && close()} />
      <VStack gap={4} padding={4}>
        <Text type="supporting">
          They sign in with a link sent to this email; there are no passwords. Payvy doesn&apos;t send anything now:
          once added, they open Payvy and ask for a link.
        </Text>
        <TextInput label="Name" value={name} onChange={setName} isRequired isDisabled={busy} />
        <TextInput label="Email" type="email" value={email} onChange={setEmail} isRequired isDisabled={busy} />
        <TextInput
          label="Recorded as"
          value={actor}
          onChange={setActor}
          placeholder={email.includes('@') ? email.split('@')[0]!.split('+')[0]!.toLowerCase() : 'the part of the email before @'}
          description="Optional. The name their actions are recorded under: a-z 0-9 . _ -"
          isDisabled={busy}
        />
        <Text type="supporting">
          Everyone here has the same rights: approving bills, releasing payments, and accepting payees and workflows.
        </Text>
        {error && <Banner status="error" title={error} />}
        <HStack gap={2} hAlign="end">
          <Button label="Cancel" variant="ghost" isDisabled={busy} onClick={close} />
          <Button label="Add person" variant="primary" isDisabled={busy || !name.trim() || !emailOk} clickAction={add} />
        </HStack>
      </VStack>
    </Dialog>
  );
}
