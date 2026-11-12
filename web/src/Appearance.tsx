import { createContext, useContext, useEffect, useState, type ReactNode } from 'react';
import { Theme } from '@astryxdesign/core/theme';
import { neutralTheme } from '@astryxdesign/theme-neutral/built';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';

type Appearance = 'light' | 'dark' | 'system';
const KEY = 'payvy.appearance';
const valid = (v: string | null): Appearance => v === 'light' || v === 'dark' ? v : 'system';
const Context = createContext<{ mode: Appearance; setMode: (mode: Appearance) => void } | null>(null);

export function AppearanceProvider({ children }: { children: ReactNode }) {
  const [mode, setMode] = useState<Appearance>(() => {
    try { return valid(localStorage.getItem(KEY)); } catch { return 'system'; }
  });
  useEffect(() => {
    const sync = (e: StorageEvent) => { if (e.key === KEY || e.key === null) setMode(valid(e.newValue)); };
    window.addEventListener('storage', sync);
    return () => window.removeEventListener('storage', sync);
  }, []);
  function choose(next: Appearance) {
    setMode(next);
    try { localStorage.setItem(KEY, next); } catch { /* Still works for this session. */ }
  }
  return <Context value={{ mode, setMode: choose }}><Theme theme={neutralTheme} mode={mode}>{children}</Theme></Context>;
}

export function AppearanceControl() {
  const appearance = useContext(Context)!;
  return <SegmentedControl label="Appearance" value={appearance.mode} onChange={v => appearance.setMode(v as Appearance)} size="sm">
      <SegmentedControlItem value="light" label="Light" />
      <SegmentedControlItem value="dark" label="Dark" />
      <SegmentedControlItem value="system" label="System" />
    </SegmentedControl>;
}
