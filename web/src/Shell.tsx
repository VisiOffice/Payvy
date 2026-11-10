import type { ReactNode } from 'react';
import { useLocation } from 'react-router';
import { AppShell } from '@astryxdesign/core/AppShell';
import { Button } from '@astryxdesign/core/Button';
import { SideNav, SideNavHeading, SideNavItem, SideNavSection } from '@astryxdesign/core/SideNav';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { useSession } from './session';

const NAV = [
  { href: '/', label: 'Waiting on you' },
  { href: '/documents', label: 'Documents' },
  { href: '/bills', label: 'All bills' },
  { href: '/workflows', label: 'Workflows' },
];

export function Shell({ children }: { children: ReactNode }) {
  const { me, signOut } = useSession();
  const { pathname } = useLocation();
  return (
    <AppShell
      contentPadding={0}
      sideNav={
        <SideNav
          header={<SideNavHeading heading="Payvy" subheading={me.organization.name} />}
          footer={
            <VStack gap={1} padding={2}>
              <Text type="supporting" maxLines={1}>
                {me.person.name}
              </Text>
              <Button label="Sign out" variant="ghost" size="sm" clickAction={signOut} />
            </VStack>
          }
        >
          <SideNavSection title="Bills" isHeaderHidden>
            {NAV.map((n) => (
              <SideNavItem
                key={n.href}
                label={n.label}
                href={n.href}
                isSelected={n.href === '/' ? pathname === '/' : pathname.startsWith(n.href)}
              />
            ))}
          </SideNavSection>
        </SideNav>
      }
    >
      {children}
    </AppShell>
  );
}
