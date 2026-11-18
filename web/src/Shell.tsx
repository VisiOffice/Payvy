import { useCan, useAccessVersion } from './permissions';
import type { CommandName } from './api/commands.gen';
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useLocation, useNavigate } from 'react-router';
import { auth, run } from './api/client';
import { AppShell } from '@astryxdesign/core/AppShell';
import { SideNav, SideNavItem, SideNavSection } from '@astryxdesign/core/SideNav';
import { DropdownMenu } from '@astryxdesign/core/DropdownMenu';
import { useSession } from './session';
import { useSetup } from './setup';
import { NavIcon } from './NavIcon';
import { Kbd } from '@astryxdesign/core/Kbd';

const NAV = [
  { href: '/', label: 'Waiting on you', icon: 'inbox' },
  { href: '/inbox', label: 'Inbox', icon: 'documents' },
  { href: '/bills', label: 'All bills', icon: 'bills' },
  { href: '/vendors', label: 'Vendors', icon: 'vendors' },
  { href: '/purchase-orders', label: 'Purchase orders', icon: 'orders' },
] as const;

/** Settings, grouped the way people look for them. A page is listed only
 * to someone who can change it; a group with nothing left is hidden. */
export const SETTINGS: { title: string; items: { href: string; label: string; command?: CommandName; hostedOnly?: boolean }[] }[] = [
  { title: 'Personal', items: [{ href: '/settings/preferences', label: 'Preferences' }] },
  {
    title: 'Company',
    items: [
      { href: '/settings/company', label: 'Company', command: 'org.update' },
      { href: '/settings/people', label: 'People' },
      { href: '/settings/agents', label: 'Agents', command: 'agent_key.create' },
      { href: '/settings/billing', label: 'Billing', command: 'billing.checkout', hostedOnly: true },
      { href: '/settings/notifications', label: 'Notifications', command: 'notification.configure' },
    ],
  },
  { title: 'Bills in', items: [{ href: '/settings/email', label: 'Email-in address', command: 'email.new_address' }] },
  {
    title: 'Paying',
    items: [
      { href: '/settings/workflows', label: 'Workflows' },
      { href: '/settings/approvals', label: 'Approvals', command: 'approval_rules.set' },
      { href: '/settings/bank', label: 'Bank', command: 'bank.connect' },
      { href: '/settings/auto-pay', label: 'Auto-pay limit', command: 'auto_pay.raise_limit' },
      { href: '/settings/purchase-orders', label: 'Purchase orders', command: 'po.configure' },
      { href: '/settings/ledger', label: 'Ledger', command: 'ledger.configure' },
    ],
  },
];

/** Typing in a field never starts a shortcut. */
function typing(e: KeyboardEvent) {
  const el = e.target as HTMLElement | null;
  return !!el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName));
}

/** G then S opens settings, as in Linear. Caught before the page's own keys,
 * so the S never reaches a page that uses it. */
function useGoToSettings(go: () => void) {
  useEffect(() => {
    let armed = 0;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented || typing(e)) return;
      const key = e.key.toLowerCase();
      if (key === 's' && armed && Date.now() - armed < 1200) {
        armed = 0;
        e.preventDefault();
        e.stopPropagation();
        go();
        return;
      }
      armed = key === 'g' && !e.shiftKey ? Date.now() : 0;
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [go]);
}

const shortcut = (...keys: string[]) => (
  <span className="payvy-menu-keys" aria-hidden="true">
    {keys.map((k, i) => <span key={k}>{i > 0 && <span className="payvy-menu-then">then</span>}<Kbd keys={k} /></span>)}
  </span>
);

/** Up to two initials, for a company mark or a person's avatar. */
const initials = (name: string) => name.trim().split(/\s+/).slice(0, 2).map(word => word[0]).join('').toUpperCase();

export function Shell({ children }: { children: ReactNode }) {
  const { me, signOut } = useSession();
  const can = useCan();
  const access = useAccessVersion();
  const { pathname, search } = useLocation();
  const navigate = useNavigate();
  const nav = NAV.filter(n => n.href !== '/purchase-orders' || me.features.purchase_orders);
  // The first-run checklist stays in the menu until every step is done.
  const setup = useSetup();
  const starting = setup.loaded && !setup.failed && !setup.done;
  const inSettings = pathname === '/settings' || pathname.startsWith('/settings/');
  // Where "Back to app" returns: the last page outside settings.
  const back = useRef('/');
  if (!inSettings) back.current = pathname + search;
  const openSettings = useCallback(() => navigate('/settings'), [navigate]);
  useGoToSettings(openSettings);
  const groups = SETTINGS
    .map(g => ({ ...g, items: g.items.filter(i => (!i.command || can(i.command)) && (!i.hostedOnly || !!me.plan?.billing)) }))
    .filter(g => g.items.length > 0);
  const billing = !!me.plan?.billing && can('billing.checkout');
  const [waiting, setWaiting] = useState<number | null>(null);

  useEffect(() => {
    let live = true;
    let request = 0;
    const refresh = () => {
      const current = ++request;
      run<{ count: number }>('inbox.get', {})
        .then(r => { if (live && current === request) setWaiting(r.result.count); })
        .catch(() => { if (live && current === request) setWaiting(null); });
    };
    refresh();
    window.addEventListener('payvy:changed', refresh);
    return () => { live = false; window.removeEventListener('payvy:changed', refresh); };
  }, [pathname, access]);
  // Other companies this address belongs to. Switching starts a new session
  // there (the server checks membership), then reloads into it.
  const others = (me.companies ?? []).filter(c => c.id !== me.organization.id);
  const switchItems = others.length ? [
    { type: 'divider' as const },
    ...others.map(c => ({
      label: `Switch to ${c.name}`,
      onClick: () => { auth.switchTo(c.id).then(() => window.location.assign('/'), () => window.location.assign('/sign-in')); },
    })),
  ] : [];
  return (
    <AppShell
      contentPadding={0}
      variant="section"
      className="payvy-shell"
      sideNav={inSettings ? (
        <SideNav
          header={<div className="payvy-settings-back"><SideNavItem
            label="Back to app"
            href={back.current}
            icon={<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="M10 3 5 8l5 5" /></svg>}
          /></div>}
        >
          {groups.map(g => (
            <SideNavSection key={g.title} title={g.title}>
              {g.items.map(i => (
                <SideNavItem key={i.href} label={i.label} href={i.href} isSelected={pathname.startsWith(i.href)} />
              ))}
            </SideNavSection>
          ))}
        </SideNav>
      ) : (
        <SideNav
          header={<div className="payvy-company-menu"><DropdownMenu
            renderTrigger={props => <button {...props} className="payvy-company-switcher" aria-label={`${me.organization.name}, company menu`}>
              <span className="payvy-company-mark" aria-hidden="true">{initials(me.organization.name)}</span>
              <span className="payvy-company-label"><strong>{me.organization.name}</strong><span>Company</span></span>
              <svg width="14" height="18" viewBox="0 0 14 18" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="m4 6 3-3 3 3m-6 6 3 3 3-3" /></svg>
            </button>}
            items={[
              { label: 'Settings', href: '/settings', endContent: shortcut('g', 's') },
              { label: 'Invite and manage people', href: '/settings/people' },
              ...switchItems,
            ]}
          /></div>}
          footer={
            <div className="payvy-nav-footer"><DropdownMenu
              placement="end"
              alignment="end"
              menuWidth={240}
              renderTrigger={props => <button {...props} className="payvy-company-switcher payvy-account-switcher" aria-label={`${me.person.name}, account menu`}>
                <span className="payvy-avatar" aria-hidden="true">{initials(me.person.name)}</span>
                <span className="payvy-company-label"><strong>{me.person.name}</strong><span>{me.person.email}</span></span>
                <svg width="14" height="18" viewBox="0 0 14 18" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="m4 6 3-3 3 3m-6 6 3 3 3-3" /></svg>
              </button>}
              header={<div className="payvy-account-header">
                <span className="payvy-avatar" aria-hidden="true">{initials(me.person.name)}</span>
                <span className="payvy-company-label"><strong>{me.person.name}</strong><span>{me.person.email}</span></span>
              </div>}
              items={[
                { label: 'Preferences', href: '/settings/preferences' },
                ...(billing ? [{ label: 'Billing', href: '/settings/billing' }] : []),
                ...(can('notification.configure') ? [{ label: 'Notifications', href: '/settings/notifications' }] : []),
                { label: 'Settings', href: '/settings', endContent: shortcut('g', 's') },
                { type: 'divider' as const },
                { label: 'Sign out', onClick: signOut },
              ]}
            /></div>
          }
        >
          <SideNavSection title="Workspace">
            {nav.map((n) => (
              <SideNavItem
                key={n.href}
                label={n.label}
                icon={<NavIcon name={n.icon} />}
                endContent={n.href === '/' && waiting !== null ? <span className="payvy-nav-count" aria-label={`${waiting} waiting`}>{waiting}</span> : undefined}
                href={n.href}
                isSelected={n.href === '/' ? pathname === '/' : pathname.startsWith(n.href)}
              />
            ))}
            {starting && <SideNavItem label="Get started" icon={<NavIcon name="start" />} href="/start" isSelected={pathname === '/start'} />}
          </SideNavSection>
        </SideNav>
      )}
    >
      {inSettings ? <div className="payvy-settings">{children}</div> : children}
    </AppShell>
  );
}
