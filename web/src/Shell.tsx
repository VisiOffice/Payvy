import { useEffect, useState, type ReactNode } from 'react';
import { useLocation } from 'react-router';
import { run } from './api/client';
import { AppShell } from '@astryxdesign/core/AppShell';
import { Button } from '@astryxdesign/core/Button';
import { SideNav, SideNavHeading, SideNavItem, SideNavSection } from '@astryxdesign/core/SideNav';
import { Text } from '@astryxdesign/core/Text';
import { HStack } from '@astryxdesign/core/HStack';
import { useSession } from './session';
import { AppearanceControl } from './Appearance';
import { NavIcon } from './NavIcon';

const NAV = [
  { href: '/', label: 'Waiting on you', icon: 'inbox' },
  { href: '/documents', label: 'Documents', icon: 'documents' },
  { href: '/bills', label: 'All bills', icon: 'bills' },
  { href: '/vendors', label: 'Vendors', icon: 'vendors' },
  { href: '/workflows', label: 'Workflows', icon: 'workflows' },
] as const;

export function Shell({ children }: { children: ReactNode }) {
  const { me, signOut } = useSession();
  const { pathname } = useLocation();
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
  }, [pathname]);
  return (
    <AppShell
      contentPadding={0}
      variant="section"
      className="payvy-shell"
      sideNav={
        <SideNav
          header={<SideNavHeading heading="Payvy" headingHref="/" subheading={me.organization.name} icon={<span className="payvy-brand-mark" aria-hidden="true">p<span>·</span></span>} />}
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
            {NAV.map((n) => (
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
            <SideNavItem label="Bank" href="/settings/bank" isSelected={pathname.startsWith('/settings/bank')} />
          </SideNavSection>
        </SideNav>
      }
    >
      {children}
    </AppShell>
  );
}
