import { Allowed, useCan, useAccessVersion } from './permissions';
import { useEffect, useState, type ReactNode } from 'react';
import { useLocation } from 'react-router';
import { run } from './api/client';
import { AppShell } from '@astryxdesign/core/AppShell';
import { Button } from '@astryxdesign/core/Button';
import { SideNav, SideNavItem, SideNavSection } from '@astryxdesign/core/SideNav';
import { Text } from '@astryxdesign/core/Text';
import { HStack } from '@astryxdesign/core/HStack';
import { DropdownMenu } from '@astryxdesign/core/DropdownMenu';
import { useSession } from './session';
import { AppearanceControl } from './Appearance';
import { NavIcon } from './NavIcon';

const NAV = [
  { href: '/', label: 'Waiting on you', icon: 'inbox' },
  { href: '/documents', label: 'Inbox', icon: 'documents' },
  { href: '/bills', label: 'All bills', icon: 'bills' },
  { href: '/vendors', label: 'Vendors', icon: 'vendors' },
  { href: '/purchase-orders', label: 'Purchase orders', icon: 'orders' },
  { href: '/workflows', label: 'Workflows', icon: 'workflows' },
] as const;

export function Shell({ children }: { children: ReactNode }) {
  const { me, signOut } = useSession();
  const can = useCan();
  const access = useAccessVersion();
  const { pathname } = useLocation();
  const nav = NAV.filter(n => n.href !== '/purchase-orders' || me.features.purchase_orders);
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
  return (
    <AppShell
      contentPadding={0}
      variant="section"
      className="payvy-shell"
      sideNav={
        <SideNav
          header={<div className="payvy-company-menu"><DropdownMenu
            renderTrigger={props => <button {...props} className="payvy-company-switcher" aria-label={`${me.organization.name}, company menu`}>
              <span className="payvy-company-mark" aria-hidden="true">{me.organization.name.trim().split(/\s+/).slice(0, 2).map(word => word[0]).join('').toUpperCase()}</span>
              <span className="payvy-company-label"><strong>{me.organization.name}</strong><span>Company</span></span>
              <svg width="14" height="18" viewBox="0 0 14 18" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="m4 6 3-3 3 3m-6 6 3 3 3-3" /></svg>
            </button>}
            items={[
              ...(can('bank.connect') ? [{ label: 'Bank settings', href: '/settings/bank' }] : []),
              ...(can('email.new_address') ? [{ label: 'Email-in address', href: '/settings/email' }] : []),
              ...(can('notification.configure') ? [{ label: 'Notifications', href: '/settings/notifications' }] : []),
              ...(can('approval_rules.set') ? [{ label: 'Approvals', href: '/settings/approvals' }] : []),
              ...(can('po.configure') ? [{ label: 'Purchase orders', href: '/settings/purchase-orders' }] : []),
              { label: 'Default workflow', href: '/workflows' },
              { label: 'People', href: '/settings/people' },
              ...(can('org.update') ? [{ label: 'Company', href: '/settings/company' }] : []),
              { type: 'divider' },
              { label: 'Sign out', onClick: signOut },
            ]}
          /></div>}
          footer={
            <div className="payvy-nav-footer">
              <AppearanceControl />
            <HStack gap={2} padding={2} hAlign="between" vAlign="center">
              <span className="payvy-avatar" aria-hidden="true">{me.person.name.slice(0, 1).toUpperCase()}</span>
              <Text type="supporting" maxLines={1}>
                {me.person.name}
              </Text>
              <Button label="Sign out" variant="ghost" size="sm" clickAction={signOut} />
            </HStack>
            </div>
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
          </SideNavSection>
          <SideNavSection title="Settings">
            <Allowed command="bank.connect"><SideNavItem label="Bank" href="/settings/bank" isSelected={pathname.startsWith('/settings/bank')} /></Allowed>
            <Allowed command="email.new_address"><SideNavItem label="Email-in address" href="/settings/email" isSelected={pathname.startsWith('/settings/email')} /></Allowed>
            <SideNavItem label="People" href="/settings/people" isSelected={pathname.startsWith('/settings/people')} />
            <Allowed command="org.update"><SideNavItem label="Company" href="/settings/company" isSelected={pathname.startsWith('/settings/company')} /></Allowed>
            <Allowed command="auto_pay.raise_limit"><SideNavItem label="Auto-pay limit" href="/settings/auto-pay" isSelected={pathname.startsWith('/settings/auto-pay')} /></Allowed>
            <Allowed command="notification.configure"><SideNavItem label="Notifications" href="/settings/notifications" isSelected={pathname.startsWith('/settings/notifications')} /></Allowed>
            <Allowed command="approval_rules.set"><SideNavItem label="Approvals" href="/settings/approvals" isSelected={pathname.startsWith('/settings/approvals')} /></Allowed>
            <Allowed command="po.configure"><SideNavItem label="Purchase orders" href="/settings/purchase-orders" isSelected={pathname.startsWith('/settings/purchase-orders')} /></Allowed>
          </SideNavSection>
        </SideNav>
      }
    >
      {children}
    </AppShell>
  );
}
