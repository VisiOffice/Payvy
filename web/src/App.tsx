import { useCallback, useEffect, useState } from 'react';
import { Navigate, Route, Routes, useLocation } from 'react-router';
import { Center } from '@astryxdesign/core/Center';
import { Spinner } from '@astryxdesign/core/Spinner';
import { ApiError, auth, type Me } from './api/client';
import { SessionContext } from './session';
import { Shell } from './Shell';
import { SignIn } from './pages/SignIn';
import { Bills } from './pages/Bills';

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
    <SessionContext value={{ me: state.me, signOut }}>
      <Shell>
        <Routes>
          <Route path="/" element={<Bills mine />} />
          <Route path="/bills" element={<Bills />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </Shell>
    </SessionContext>
  );
}
