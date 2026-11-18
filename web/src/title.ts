// The browser tab's title: the page, then the company, then Payvy, so tabs
// for several pages (or companies) tell themselves apart.
import { useContext, useEffect } from 'react';
import { SessionContext } from './session';

/** Sets document.title while the page shows. Undefined or empty leaves it. */
export function usePageTitle(title: string | undefined) {
  const org = useContext(SessionContext)?.me.organization.name;
  useEffect(() => {
    if (!title) return;
    document.title = [title, org, 'Payvy'].filter(Boolean).join(' · ');
  }, [title, org]);
}
