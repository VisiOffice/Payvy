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
