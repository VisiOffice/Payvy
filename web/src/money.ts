const usd = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' });

/** Payvy keeps money as integer cents. */
export function money(cents: number): string {
  return usd.format(cents / 100);
}

export function date(iso: string | null | undefined): string {
  if (!iso) return '';
  // Dates without a time are calendar dates: don't shift them by time zone.
  const d = iso.length === 10 ? new Date(`${iso}T00:00:00`) : new Date(iso);
  return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' });
}

/** "1,234.56" or "$1234.5" → cents; null if it isn't an amount. */
export function cents(dollars: string): number | null {
  const m = /^\$?\s*(\d{1,3}(?:,\d{3})*|\d+)(?:\.(\d{1,2}))?$/.exec(dollars.trim());
  if (!m) return null;
  const whole = Number(m[1]!.replaceAll(',', ''));
  const frac = Number((m[2] ?? '0').padEnd(2, '0'));
  return whole * 100 + frac;
}

/** Cents as an editable amount: 123456 → "1234.56". */
export const dollars = (c: number) => (c / 100).toFixed(2);
