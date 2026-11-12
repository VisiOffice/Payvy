// Settings laid out as Linear does: a titled card of rows, each a label and
// what it does on the left, the control on the right.
import type { ReactNode } from 'react';

export function SettingsGroup({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="payvy-settings-group" aria-label={title}>
      <h2>{title}</h2>
      <div className="payvy-settings-card">{children}</div>
    </section>
  );
}

export function SettingsRow({ label, description, children }: { label: string; description?: ReactNode; children: ReactNode }) {
  return (
    <div className="payvy-settings-row">
      <div>
        <div className="payvy-settings-label">{label}</div>
        {description && <div className="payvy-settings-description">{description}</div>}
      </div>
      <div className="payvy-settings-control">{children}</div>
    </div>
  );
}
