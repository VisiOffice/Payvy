// What a new company has left to do, for the first-run checklist (/start),
// the menu entry that leads to it, and where `/` lands. Every step is read
// from what exists: a document or bill, a bank that pays, a second person, a
// plan. Nothing here is stored, so a step can't be ticked by looking at it.
// Only "skip for now" is remembered, in this browser, per company.
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { run } from './api/client';
import { isLive, type Billing } from './billing';
import { useSession } from './session';

export interface Setup {
  /** Every read has answered. Until then nothing is known and nobody is sent anywhere. */
  loaded: boolean;
  /** A read failed: the steps can't be trusted, so `/` behaves as if all were done. */
  failed: boolean;
  /** A document to review or filed, or a bill, exists. */
  bill: boolean;
  /** The account of the live connection that can pay (Mercury), if there is one. */
  bank: { account_name: string } | null;
  /** How many people can sign in. */
  people: number;
  /** Hosted with billing only (`null` elsewhere): a plan is in force that
   *  isn't the trial. `unknown` while billing can't be reached. */
  plan: 'done' | 'todo' | 'unknown' | null;
  /** billing.get, on hosted with billing. */
  billing: Billing | null;
  /** Every step that applies here is done (an unknown plan counts as done). */
  done: boolean;
  /** Reads everything again, unless it was read a moment ago. */
  reload: () => Promise<void>;
}

interface Local {
  bill: boolean;
  bank: { account_name: string } | null;
  people: number;
}

const NOTHING: Setup = {
  loaded: false, failed: false, bill: false, bank: null, people: 0, plan: null, billing: null, done: false,
  reload: async () => undefined,
};
const SetupContext = createContext<Setup>(NOTHING);
export const useSetup = () => useContext(SetupContext);

/** One row is enough to know something exists. */
async function readLocal(): Promise<Local> {
  const one = { limit: 1 };
  const [inbox, filed, bills, bank, people] = await Promise.all([
    run<{ documents: unknown[] }>('document.list', { status: 'inbox', ...one }),
    run<{ documents: unknown[] }>('document.list', { status: 'filed', ...one }),
    run<{ bills: unknown[] }>('bill.list', one),
    run<{ connections: { capability: string; account_name: string }[] }>('bank.get', {}),
    run<{ people: unknown[] }>('person.list', {}),
  ]);
  const paying = bank.result.connections.find((c) => c.capability === 'pay');
  return {
    bill: inbox.result.documents.length + filed.result.documents.length + bills.result.bills.length > 0,
    bank: paying ? { account_name: paying.account_name } : null,
    people: people.result.people.length,
  };
}

/** A plan step is done when a subscription is live, or payments are on by
 *  something other than the trial (a plan granted by the operator). */
function planStep(billing: Billing | null): Setup['plan'] {
  if (!billing || !billing.available) return 'unknown';
  if (isLive(billing.subscription)) return 'done';
  return billing.plan.payments && !billing.trial?.running ? 'done' : 'todo';
}

export function SetupProvider({ children }: { children: ReactNode }) {
  const { me } = useSession();
  const org = me.organization.id;
  const hasBilling = !!me.plan?.billing;
  const [local, setLocal] = useState<Local | null>(null);
  // undefined: not asked yet. null: asked, and it couldn't answer.
  const [billing, setBilling] = useState<Billing | null | undefined>(undefined);
  const [failed, setFailed] = useState(false);
  const request = useRef(0);
  /** When everything was last read. */
  const fresh = useRef(0);

  const load = useCallback(async (withBilling: boolean) => {
    const current = ++request.current;
    const [l, b] = await Promise.all([
      readLocal().catch(() => null),
      withBilling && hasBilling ? run<Billing>('billing.get', {}).then((r) => r.result, () => null) : undefined,
    ]);
    if (current !== request.current) return;
    if (withBilling) fresh.current = Date.now();
    setFailed(!l);
    if (l) setLocal(l);
    if (b !== undefined) setBilling(b);
  }, [hasBilling]);

  // A new company (after a switch) starts from nothing.
  useEffect(() => {
    setLocal(null);
    setBilling(undefined);
    setFailed(false);
    void load(true);
  }, [org, load]);

  const loaded = (!!local || failed) && (!hasBilling || billing !== undefined);
  const plan = hasBilling ? planStep(billing ?? null) : null;
  const done = !!local && local.bill && !!local.bank && local.people > 1 && plan !== 'todo';

  // While something is left, a write anywhere in the app may have done it.
  // Billing isn't asked again for that: nothing in the app changes it.
  useEffect(() => {
    if (done) return;
    let timer = 0;
    const changed = () => {
      clearTimeout(timer);
      timer = window.setTimeout(() => void load(false), 400);
    };
    window.addEventListener('payvy:changed', changed);
    return () => { clearTimeout(timer); window.removeEventListener('payvy:changed', changed); };
  }, [done, load]);

  // The checklist asks again when it opens; not if `/` only just did.
  const reload = useCallback(async () => {
    if (Date.now() - fresh.current > 2000) await load(true);
  }, [load]);
  const value = useMemo<Setup>(() => ({
    loaded,
    failed,
    bill: local?.bill ?? false,
    bank: local?.bank ?? null,
    people: local?.people ?? 0,
    plan,
    billing: billing ?? null,
    done,
    reload,
  }), [loaded, failed, local, plan, billing, done, reload]);
  return <SetupContext value={value}>{children}</SetupContext>;
}

// "Skip for now" is a convenience for this browser, not a fact about the
// company: it only decides where `/` lands. Storage can be blocked or
// cleared, so every use is guarded and the page works without it.
const key = (org: string) => `payvy:start-skipped:${org}`;
const skippedHere = new Set<string>();

export function skipped(org: string): boolean {
  if (skippedHere.has(org)) return true;
  try {
    return localStorage.getItem(key(org)) === '1';
  } catch {
    return false;
  }
}

export function skip(org: string) {
  skippedHere.add(org);
  try {
    localStorage.setItem(key(org), '1');
  } catch {
    // Remembered until the tab closes, then asked again.
  }
}

/** Whole days until the trial ends, counting a part of a day as one. */
export function daysLeft(endsAt: string, now = Date.now()): number {
  return Math.max(1, Math.ceil((new Date(endsAt).getTime() - now) / 86_400_000));
}
