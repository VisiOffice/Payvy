import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { Navigate, Route, Routes, useLocation, useParams } from 'react-router';
import { Center } from '@astryxdesign/core/Center';
import { Spinner } from '@astryxdesign/core/Spinner';
import { ApiError, auth, type Me } from './api/client';
import { SessionContext } from './session';
import { Shell } from './Shell';
import { SignIn } from './pages/SignIn';
import { Bills } from './pages/Bills';
import { Inbox } from './pages/Inbox';
import { Documents } from './pages/Documents';
import { Vendors } from './pages/Vendors';

// The bill page brings pdf.js; load it only when a bill is opened.
const BillPage = lazy(() => import('./pages/Bill').then((m) => ({ default: m.BillPage })));
const Workflows = lazy(() => import('./pages/Workflows').then((m) => ({ default: m.Workflows })));
const Bank = lazy(() => import('./pages/Bank').then((m) => ({ default: m.Bank })));
const EmailSettings = lazy(() => import('./pages/EmailSettings').then((m) => ({ default: m.EmailSettings })));
const PeopleSettings = lazy(() => import('./pages/PeopleSettings').then((m) => ({ default: m.PeopleSettings })));
const CompanySettings = lazy(() => import('./pages/CompanySettings').then((m) => ({ default: m.CompanySettings })));
const AutoPaySettings = lazy(() => import('./pages/AutoPaySettings').then((m) => ({ default: m.AutoPaySettings })));
const VendorPage = lazy(() => import('./pages/Vendor').then((m) => ({ default: m.VendorPage })));
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
      <Shell>
        <Routes>
          <Route path="/" element={<Inbox />} />
          <Route path="/bills" element={<Bills />} />
          <Route
            path="/bills/:id"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <BillPage />
              </Suspense>
            }
          />
          <Route path="/documents" element={<Documents />} />
          <Route
            path="/documents/:id"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <DocumentReview />
              </Suspense>
            }
          />
          <Route
            path="/workflows"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <Workflows />
              </Suspense>
            }
          />
          <Route path="/workflows/:key" element={<OldWorkflowLink />} />
          <Route path="/vendors" element={<Vendors />} />
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
                <Bank />
              </Suspense>
            }
          />
          <Route
            path="/settings/email"
            element={
              <Suspense fallback={<Spinner label="Loading" />}>
                <EmailSettings />
              </Suspense>
            }
          />
          <Route path="/settings/people" element={<Suspense fallback={<Spinner label="Loading" />}><PeopleSettings /></Suspense>} />
          <Route path="/settings/company" element={<Suspense fallback={<Spinner label="Loading" />}><CompanySettings /></Suspense>} />
          <Route path="/settings/auto-pay" element={<Suspense fallback={<Spinner label="Loading" />}><AutoPaySettings /></Suspense>} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </Shell>
    </SessionContext>
  );
}

/** A vendor's workflow used to have its own page; it lives on the vendor's page now. */
function OldWorkflowLink() {
  const { key } = useParams();
  return <Navigate to={key === 'default' ? '/workflows' : `/vendors/${key}#workflow`} replace />;
}
