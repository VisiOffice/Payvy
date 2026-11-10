import { useEffect, useState, type ReactNode } from 'react';
import { useLocation } from 'react-router';
import { run } from './api/client';
import { AppShell } from '@astryxdesign/core/AppShell';
import { Button } from '@astryxdesign/core/Button';
import { SideNav, SideNavHeading, SideNavItem, SideNavSection } from '@astryxdesign/core/SideNav';
import { Text } from '@astryxdesign/core/Text';
import { HStack } from '@astryxdesign/core/HStack';
import { useSession } from './session';

const NAV = [
  { href: '/', label: 'Waiting on you' },
  { href: '/documents', label: 'Documents' },
  { href: '/bills', label: 'All bills' },
  { href: '/vendors', label: 'Vendors' },
  { href: '/workflows', label: 'Workflows' },
];

export function Shell({ children }: { children: ReactNode }) {
  const { me, signOut } = useSession();
  const { pathname } = useLocation();
  const [waiting, setWaiting] = useState<number | null>(null);

  // The inbox count rides on the nav label; refreshed on every navigation.
  useEffect(() => {
    let live = true;
    run<{ count: number }>('inbox.get', {})
      .then((r) => live && setWaiting(r.result.count))
      .catch(() => live && setWaiting(null));
    return () => {
      live = false;
    };
  }, [pathname]);
  return (
    <AppShell
      contentPadding={0}
      sideNav={
        <SideNav
          header={<SideNavHeading heading="Payvy" subheading={me.organization.name} />}
          footer={
            <HStack gap={2} padding={2} hAlign="between" vAlign="center">
              <Text type="supporting" maxLines={1}>
                {me.person.name}
              </Text>
              <Button label="Sign out" variant="ghost" size="sm" clickAction={signOut} />
            </HStack>
          }
        >
          <SideNavSection title="Bills" isHeaderHidden>
            {NAV.map((n) => (
              <SideNavItem
                key={n.href}
                label={n.href === '/' && waiting ? `${n.label} (${waiting})` : n.label}
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
