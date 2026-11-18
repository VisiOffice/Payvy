import { Access, useCan } from './permissions';
import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { Navigate, Route, Routes, useLocation, useNavigationType, useParams } from 'react-router';
import { Center } from '@astryxdesign/core/Center';
import { Spinner } from '@astryxdesign/core/Spinner';
import { ApiError, auth, type Me } from './api/client';
import { SessionContext, useSession } from './session';
import { SetupProvider, skipped, useSetup } from './setup';
import { Shell } from './Shell';
import { SignIn } from './pages/SignIn';
import { Bills } from './pages/Bills';
import { Inbox } from './pages/Inbox';
import { Documents } from './pages/Documents';
import { DocumentLoading } from './documents/Loading';
import { Vendors } from './pages/Vendors';

// The bill page brings pdf.js; load it only when a bill is opened.
/** /documents/:id → /inbox/:id, keeping any query or hash. */
function MovedDocument() {
  const { id } = useParams();
  const { search, hash } = useLocation();
  return <Navigate to={`/inbox/${id ?? ''}${search}${hash}`} replace />;
}

const BillPage = lazy(() => import('./pages/Bill').then((m) => ({ default: m.BillPage })));
const Workflows = lazy(() => import('./pages/Workflows').then((m) => ({ default: m.Workflows })));
const Preferences = lazy(() => import('./pages/Preferences').then((m) => ({ default: m.Preferences })));
const Bank = lazy(() => import('./pages/Bank').then((m) => ({ default: m.Bank })));
const EmailSettings = lazy(() => import('./pages/EmailSettings').then((m) => ({ default: m.EmailSettings })));
const PeopleSettings = lazy(() => import('./pages/PeopleSettings').then((m) => ({ default: m.PeopleSettings })));
const CompanySettings = lazy(() => import('./pages/CompanySettings').then((m) => ({ default: m.CompanySettings })));
const FirstRun = lazy(() => import('./pages/FirstRun').then((m) => ({ default: m.FirstRun })));
const BillingSettings = lazy(() => import('./pages/BillingSettings').then((m) => ({ default: m.BillingSettings })));
const AutoPaySettings = lazy(() => import('./pages/AutoPaySettings').then((m) => ({ default: m.AutoPaySettings })));
const NotificationSettings = lazy(() => import('./pages/NotificationSettings').then(m => ({ default: m.NotificationSettings })));
const ApprovalSettings = lazy(() => import('./pages/ApprovalSettings').then((m) => ({ default: m.ApprovalSettings })));
const VendorPage = lazy(() => import('./pages/Vendor').then((m) => ({ default: m.VendorPage })));
const PurchaseOrderSettings = lazy(() => import('./pages/PurchaseOrderSettings').then((m) => ({ default: m.PurchaseOrderSettings })));
const AgentSettings = lazy(() => import('./pages/AgentSettings').then((m) => ({ default: m.AgentSettings })));
const LedgerSettings = lazy(() => import('./pages/LedgerSettings').then((m) => ({ default: m.LedgerSettings })));
const PurchaseOrders = lazy(() => import('./pages/PurchaseOrders').then((m) => ({ default: m.PurchaseOrders })));
const DocumentReview = lazy(() =>
  import('./pages/DocumentReview').then((m) => ({ default: m.DocumentReview })),
);

type State = { kind: 'loading' } | { kind: 'signed_out' } | { kind: 'signed_in'; me: Me };

export function App() {
  const [state, setState] = useState<State>({ kind: 'loading' });
  const location = useLocation();

  useEffect(() => {
    auth
      .me()
      .then((me) => setState({ kind: 'signed_in', me }))
      .catch((e) => {
        if (!(e instanceof ApiError && e.status === 401)) console.error(e);
        setState({ kind: 'signed_out' });
      });
  }, []);

  useEffect(() => {
    if (state.kind !== 'signed_in') return;
    let live = true;
    const refresh = () => { void auth.me().then(me => { if (live) setState({ kind: 'signed_in', me }); }).catch(e => { if (live && e instanceof ApiError && e.status === 401) setState({ kind: 'signed_out' }); }); };
    refresh();
    window.addEventListener('focus', refresh);
    window.addEventListener('payvy:changed', refresh);
    window.addEventListener('payvy:access-changed', refresh);
    const timer = window.setInterval(refresh, 30_000);
    return () => { live = false; clearInterval(timer); window.removeEventListener('focus', refresh); window.removeEventListener('payvy:changed', refresh); window.removeEventListener('payvy:access-changed', refresh); };
  }, [state.kind, location.pathname]);

  const signOut = useCallback(async () => {
    await auth.signOut();
    setState({ kind: 'signed_out' });
  }, []);

  if (state.kind === 'loading') {
    return (
      <Center height="100%">
        <Spinner size="lg" label="Loading" />
      </Center>
    );
  }

  if (state.kind === 'signed_out' || location.pathname === '/sign-in') {
    return (
      <Routes>
        <Route
          path="/sign-in"
          element={<SignIn onSignedIn={(me) => setState({ kind: 'signed_in', me })} />}
        />
        <Route
          path="*"
          element={
            <Navigate
              to="/sign-in"
              replace
              state={{ from: location.pathname + location.search }}
            />
          }
        />
      </Routes>
    );
  }

  return (
    <SessionContext value={{ me: state.me, signOut, setMe: (me) => setState({ kind: 'signed_in', me }) }}>
      <SetupProvider>
      <Shell>
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/start" element={<Suspense fallback={<Spinner label="Loading" />}><FirstRun /></Suspense>} />
          <Route path="/bills" element={<Bills />} />
          <Route
            path="/bills/:id"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <BillPage />
              </Suspense>
            }
          />
          <Route path="/inbox" element={<Documents />} />
          <Route
            path="/inbox/:id"
            element={
              <Suspense fallback={<DocumentLoading />}>
                <DocumentReview key={location.pathname} />
              </Suspense>
            }
          />
          {/* The inbox was /documents: older links, bookmarks and history still open it. */}
          <Route path="/documents" element={<Navigate to="/inbox" replace />} />
          <Route path="/documents/:id" element={<MovedDocument />} />
          {/* Workflows moved into settings; older links still open them. */}
          <Route path="/workflows" element={<Navigate to="/settings/workflows" replace />} />
          <Route path="/settings" element={<Navigate to="/settings/preferences" replace />} />
          <Route path="/settings/preferences" element={<Suspense fallback={<Spinner label="Loading" />}><Preferences /></Suspense>} />
          <Route
            path="/settings/workflows"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <Workflows />
              </Suspense>
            }
          />
          <Route path="/workflows/:key" element={<OldWorkflowLink />} />
          <Route path="/vendors" element={<Vendors />} />
          <Route
            path="/purchase-orders"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <PurchaseOrders />
              </Suspense>
            }
          />
          <Route
            path="/vendors/:id"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <VendorPage />
              </Suspense>
            }
          />
          <Route
            path="/settings/bank"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <Access command="bank.connect"><Bank /></Access>
              </Suspense>
            }
          />
          <Route
            path="/settings/email"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <Access command="email.new_address"><EmailSettings /></Access>
              </Suspense>
            }
          />
          <Route path="/settings/people" element={<Suspense fallback={<Spinner label="Loading" />}><PeopleSettings /></Suspense>} />
          <Route path="/settings/billing" element={<Suspense fallback={<Spinner label="Loading" />}><Access command="billing.checkout"><BillingSettings /></Access></Suspense>} />
          <Route path="/settings/company" element={<Suspense fallback={<Spinner label="Loading" />}><Access command="org.update"><CompanySettings /></Access></Suspense>} />
          <Route path="/settings/auto-pay" element={<Suspense fallback={<Spinner label="Loading" />}><Access command="auto_pay.raise_limit"><AutoPaySettings /></Access></Suspense>} />
          <Route path="/settings/notifications" element={<Suspense fallback={<Spinner label="Loading" />}><Access command="notification.configure"><NotificationSettings /></Access></Suspense>} />
          <Route path="/settings/purchase-orders" element={<Suspense fallback={<Spinner label="Loading" />}><Access command="po.configure"><PurchaseOrderSettings /></Access></Suspense>} />
          <Route path="/settings/agents" element={<Suspense fallback={<Spinner label="Loading" />}><Access command="agent_key.create"><AgentSettings /></Access></Suspense>} />
          <Route path="/settings/ledger" element={<Suspense fallback={<Spinner label="Loading" />}><Access command="ledger.configure"><LedgerSettings /></Access></Suspense>} />
          <Route
            path="/settings/approvals"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <Access command="approval_rules.set"><ApprovalSettings /></Access>
              </Suspense>
            }
          />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </Shell>
      </SetupProvider>
    </SessionContext>
  );
}

/** A vendor's workflow used to have its own page; it lives on the vendor's page now. */
function OldWorkflowLink() {
  const { key } = useParams();
  return <Navigate to={key === 'default' ? '/settings/workflows' : `/vendors/${key}#workflow`} replace />;
}

/** Where `/` lands. Waiting on you, except:
 *  - A hosted company whose payments are off (its trial ended with no plan,
 *    or it never had one) sends its Admins to Settings > Billing.
 *  - A company with setup left to do sends whoever can do some of it to the
 *    first-run checklist (/start), until every step is done or they skip it
 *    in this browser. A company with every step done never goes there.
 *  Following a link inside the app to `/` always shows Waiting on you, so
 *  the checklist never stands between a person and their work; it is where
 *  they arrive (a sign-in link, a typed address, a reload). */
function Home() {
  const { me } = useSession();
  const can = useCan();
  const setup = useSetup();
  const arrived = useNavigationType() !== 'PUSH';
  const waiting = <Center height="100%"><Spinner size="lg" label="Loading" /></Center>;

  if (me.plan?.billing && !me.plan.payments && can('billing.checkout')) {
    // A new company's trial reaches billing.get before its plan reaches this
    // server: while it runs, the company is new, not lapsed.
    if (!setup.loaded) return waiting;
    if (!setup.billing?.trial?.running) return <Navigate to="/settings/billing" replace />;
  }
  if (!arrived || skipped(me.organization.id)) return <Inbox />;
  if (!setup.loaded) return waiting;
  if (setup.failed || setup.done) return <Inbox />;
  const canDoSome =
    (!setup.bill && can('document.upload'))
    || (!setup.bank && can('bank.connect'))
    || (setup.people < 2 && can('person.add'))
    || (setup.plan === 'todo' && can('billing.checkout'));
  return canDoSome ? <Navigate to="/start" replace /> : <Inbox />;
}
