import { createContext, useContext } from 'react';
import type { Me } from './api/client';

export interface Session {
  me: Me;
  signOut: () => Promise<void>;
}

export const SessionContext = createContext<Session | null>(null);

/** The signed-in person. Only used under the signed-in part of the app. */
export function useSession(): Session {
  const s = useContext(SessionContext);
  if (!s) throw new Error('useSession outside a signed-in page');
  return s;
}
