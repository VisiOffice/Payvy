import type { ReactNode } from 'react';
import type { CommandName } from './api/commands.gen';
import { useSession } from './session';
import { Banner } from '@astryxdesign/core/Banner';

export type Role = 'admin' | 'ap_clerk' | 'approver' | 'payment_releaser';
export const ROLES: { value: Role; label: string }[] = [
  { value: 'admin', label: 'Admin' }, { value: 'ap_clerk', label: 'AP clerk' },
  { value: 'approver', label: 'Approver' }, { value: 'payment_releaser', label: 'Payment releaser' },
];
export function useCan() {
  const { me } = useSession();
  return (command: CommandName) => me.commands?.includes(command) ?? false;
}
export function Allowed({ command, children }: { command: CommandName; children: ReactNode }) {
  return useCan()(command) ? children : null;
}
export function Access({ command, children }: { command: CommandName; children: ReactNode }) {
  return useCan()(command) ? children : <div style={{ padding: 24 }}><Banner status="info" title="You don't have access to these settings" description="Ask a company admin to review your roles in People." /></div>;
}

/** Stable across session refreshes until effective permissions change. */
export function useAccessVersion() {
  return useSession().me.commands?.join(',') ?? '';
}
