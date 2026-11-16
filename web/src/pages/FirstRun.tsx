// /start: the first thing a new company sees. It says the company exists and
// lists what gets it paying bills, each step ticked from what is really
// there (setup.tsx). Hosted Payvy adds the trial and the plans; a self-hosted
// install has neither. `/` lands here until every step is done or the person
// skips it, and the menu keeps it until every step is done.
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useNavigate } from 'react-router';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { HStack } from '@astryxdesign/core/HStack';
import { Link } from '@astryxdesign/core/Link';
import { Spinner } from '@astryxdesign/core/Spinner';
import { Text } from '@astryxdesign/core/Text';
import { useToast } from '@astryxdesign/core/Toast';
import { VStack } from '@astryxdesign/core/VStack';
import { ApiError, run } from '../api/client';
import { isLive, PlanCards } from '../billing';
import { ReaderLocked, useReaderLocked } from '../documents/ReaderLocked';
import { ACCEPT, base64, chosen } from '../documents/upload';
import { date } from '../money';
import { Page } from '../Page';
import { useCan } from '../permissions';
import { useSession } from '../session';
import { daysLeft, skip, useSetup } from '../setup';

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Set when this page sends the browser to Checkout, so the return can wait
 *  for Stripe's confirmation to reach the plan. Only a hint for this tab. */
const CHECKOUT = 'payvy:checkout-opened';
function takeCheckoutMark(): boolean {
  try {
    const marked = sessionStorage.getItem(CHECKOUT) === '1';
    sessionStorage.removeItem(CHECKOUT);
    return marked;
  } catch {
    return false;
  }
}
function markCheckout() {
  try {
    sessionStorage.setItem(CHECKOUT, '1');
  } catch {
    // Without it the step is ticked on the next look, not at once.
  }
}

function Step({ id, title, done, children }: { id?: string; title: string; done: boolean; children: ReactNode }) {
  return (
    <li id={id} className="payvy-setup-step" data-done={done || undefined} aria-label={`${title}: ${done ? 'done' : 'not done'}`}>
      <span className="payvy-setup-step-mark" aria-hidden="true">
        {done && <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m2.5 6.5 2.5 2.5 4.5-5.5" /></svg>}
      </span>
      <div className="payvy-setup-step-body">
        <h2>{title}{done && <span className="payvy-setup-step-done">Done</span>}</h2>
        {children}
      </div>
    </li>
  );
}

/** The company's email-in address: null until created; `unavailable` where
 *  the server takes no email. */
type Address = { kind: 'loading' } | { kind: 'unavailable' } | { kind: 'known'; address: string | null };

export function FirstRun() {
  const { me } = useSession();
  const can = useCan();
  const setup = useSetup();
  const navigate = useNavigate();
  const toast = useToast();
  const readerLocked = useReaderLocked();
  const [address, setAddress] = useState<Address>({ kind: 'loading' });
  const [creating, setCreating] = useState(false);
  const [uploading, setUploading] = useState<{ title: string; detail: string } | null>(null);
  const [planError, setPlanError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const file = useRef<HTMLInputElement>(null);
  const { reload } = setup;

  const loadAddress = useCallback(() => {
    run<{ address: string | null }>('email.address', {})
      .then((r) => setAddress({ kind: 'known', address: r.result.address }))
      // No email intake on this server, or it couldn't say: uploading still works.
      .catch((e) => setAddress(e instanceof ApiError && e.code === 'not_configured' ? { kind: 'unavailable' } : { kind: 'known', address: null }));
  }, []);
  useEffect(loadAddress, [loadAddress]);

  // What was true when the app loaded may be old by the time this opens.
  useEffect(() => {
    void reload();
    const again = () => void reload();
    window.addEventListener('focus', again);
    return () => window.removeEventListener('focus', again);
  }, [reload]);

  // Back from Checkout: the plan changes when Stripe's confirmation reaches
  // the server, a moment after the browser returns. Look again for a while.
  useEffect(() => {
    if (!takeCheckoutMark()) return;
    setConfirming(true);
    let looks = 0;
    const timer = window.setInterval(() => {
      looks += 1;
      if (looks >= 10) { clearInterval(timer); setConfirming(false); }
      void reload();
    }, 3000);
    return () => clearInterval(timer);
  }, [reload]);
  useEffect(() => { if (setup.plan === 'done') setConfirming(false); }, [setup.plan]);

  async function createAddress() {
    setCreating(true);
    try {
      const r = await run<{ address: string }>('email.new_address', {});
      setAddress({ kind: 'known', address: r.result.address });
      toast({ body: 'Email-in address created' });
    } catch (e) {
      toast({ body: message(e), type: 'error' });
    } finally {
      setCreating(false);
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast({ body: 'Copied' });
    } catch {
      toast({ body: "Couldn't copy. Select the address and copy it instead.", type: 'error' });
    }
  }

  /** As the Inbox does it: one bill opens for review, several are listed there. */
  async function upload(list: File[]) {
    const { ok, problems } = chosen(list);
    if (problems.length) toast({ body: `${problems.join('. ')}.`, type: 'error' });
    if (!ok.length) return;
    const added: string[] = [];
    try {
      for (const [index, f] of ok.entries()) {
        setUploading({ title: `Uploading ${index + 1} of ${ok.length}`, detail: f.name });
        const r = await run<{ id: string }>('document.upload', { filename: f.name, content_base64: await base64(f) });
        added.push(r.result.id);
      }
      if (added.length === 1) {
        if (!readerLocked) {
          setUploading({ title: 'Invoice uploaded. Reading the details…', detail: 'We’ll open it for review when the reading finishes.' });
          await run('document.extract', { document_id: added[0]! }).catch(() => undefined);
        }
        navigate(`/inbox/${added[0]}`);
        return;
      }
      toast({ body: `Added ${added.length} documents. They're in the Inbox.` });
    } catch (e) {
      toast({ body: added.length ? `Added ${added.length}, then stopped: ${message(e)}` : message(e), type: 'error' });
    } finally {
      setUploading(null);
    }
  }

  const billing = setup.billing;
  const trial = billing?.trial ?? null;
  const live = isLive(billing?.subscription);
  const inTrial = !!trial?.running && !live;
  const trialEnded = !!trial && !trial.running && !live && !billing?.plan.payments;
  const days = trial ? daysLeft(trial.ends_at) : 0;
  const left = `${days} ${days === 1 ? 'day' : 'days'} left`;
  const current = billing?.offers.find((o) => o.id === billing.subscription?.plan);
  const admin = can('billing.checkout');

  return (
    <Page
      narrow
      title={`${me.organization.name} is ready`}
      description="Your company was created. Here is what to do first."
      actions={!setup.done && <Button label="Skip for now" variant="ghost" size="sm" onClick={() => { skip(me.organization.id); navigate('/'); }} />}
    >
      {!setup.loaded && <Spinner label="Loading" />}
      {setup.failed && <Banner status="error" title="Couldn't check what's set up" description="The steps below may be out of date." endContent={<Button label="Try again" variant="secondary" onClick={() => void reload()} />} />}
      {setup.loaded && inTrial && <Banner status="info" title={`Trial: ${left}.`} description="No card needed until then." />}
      {setup.loaded && trialEnded && trial && <Banner status="warning" title={`Your trial ended on ${date(trial.ends_at)}.`} description="Choose a plan to send payments again." />}
      {setup.loaded && setup.done && <Banner status="success" title="Everything here is done" description="Bills that need you are in Waiting on you." endContent={<Button label="Open Waiting on you" variant="secondary" onClick={() => navigate('/')} />} />}
      {setup.loaded && (
        <ol className="payvy-setup">
          <Step title="Send the first bill" done={setup.bill}>
            <Text type="supporting">Email it or upload it. Either way it lands in the Inbox for review.</Text>
            {address.kind === 'known' && address.address && (
              <HStack gap={3} vAlign="center" wrap="wrap">
                <code className="payvy-inbox-address" aria-label="Email-in address">{address.address}</code>
                <Button label="Copy" variant="secondary" size="sm" onClick={() => void copy(address.address!)} />
              </HStack>
            )}
            {address.kind === 'known' && !address.address && (can('email.new_address')
              ? <Text type="supporting">No email-in address yet. Create one to have vendors send bills straight to Payvy.</Text>
              : <Text type="supporting">No email-in address yet. Ask an admin to create one.</Text>)}
            <HStack gap={2} wrap="wrap">
              {can('document.upload') && <Button label="Upload a bill" variant={setup.bill ? 'secondary' : 'primary'} tooltip="PDFs, scans, or photos, up to 25 MB each" isDisabled={!!uploading} onClick={() => file.current?.click()} />}
              {address.kind === 'known' && !address.address && can('email.new_address') && <Button label="Create email-in address" variant="secondary" isLoading={creating} onClick={() => void createAddress()} />}
            </HStack>
            <input ref={file} type="file" className="payvy-sr-only" accept={ACCEPT} multiple tabIndex={-1} aria-hidden="true" onChange={(e) => { void upload(Array.from(e.target.files ?? [])); e.target.value = ''; }} />
            {uploading && <div role="status" className="payvy-upload-progress"><Spinner size="sm" /><div><strong>{uploading.title}</strong><p>{uploading.detail}</p></div></div>}
            {readerLocked && (
              <VStack gap={1}>
                <Text type="supporting">Your own agent reads what arrives, or you enter the bill by hand.</Text>
                <ReaderLocked />
              </VStack>
            )}
          </Step>

          <Step title="Connect the bank that pays" done={!!setup.bank}>
            {setup.bank ? (
              <Text type="supporting">Mercury · {setup.bank.account_name}</Text>
            ) : (
              <>
                <Text type="supporting">Mercury is the bank that pays today. Payvy tells it what to pay and never holds your money.</Text>
                {can('bank.connect')
                  ? <HStack><Button label="Connect Mercury" variant="secondary" href="/settings/bank" /></HStack>
                  : <Text type="supporting">Ask an admin to connect it.</Text>}
              </>
            )}
          </Step>

          <Step title="Invite the people who approve" done={setup.people > 1}>
            {setup.people > 1 ? (
              <Text type="supporting">{setup.people} people can sign in. <Link href="/settings/people">People</Link></Text>
            ) : (
              <>
                <Text type="supporting">Add them with the roles they need. Payvy doesn't email them: they ask for their own link on the sign-in page.</Text>
                {can('person.add')
                  ? <HStack><Button label="Add people" variant="secondary" href="/settings/people" /></HStack>
                  : <Text type="supporting">Ask an admin to add them.</Text>}
              </>
            )}
          </Step>

          {setup.plan && (
            <Step id="plan" title="Choose a plan" done={setup.plan === 'done'}>
              {setup.plan === 'unknown' && <Text type="supporting">Billing can't be reached right now. Nothing about your plan has changed.</Text>}
              {setup.plan === 'done' && (
                <Text type="supporting">
                  {current ? `${current.name} · ${current.price}` : 'A plan is in force.'}
                  {admin && <> <Link href="/settings/billing">Billing</Link></>}
                </Text>
              )}
              {setup.plan === 'todo' && (
                <>
                  {inTrial && <Text type="supporting">Not needed during the trial: {left}.</Text>}
                  {confirming && <div role="status" className="payvy-loading-message"><Spinner size="sm" />Waiting for Stripe to confirm the plan</div>}
                  {planError && <Banner status="error" title="Couldn't open billing" description={planError} />}
                  {admin
                    ? <div className="payvy-setup-plans"><PlanCards offers={billing?.offers ?? []} returnTo="start" headingLevel={3} onOpening={markCheckout} onError={setPlanError} /></div>
                    : <Text>Ask an admin to choose a plan.</Text>}
                </>
              )}
            </Step>
          )}
        </ol>
      )}
    </Page>
  );
}
