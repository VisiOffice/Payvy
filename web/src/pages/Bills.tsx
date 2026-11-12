import { useEffect, useMemo, useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { EmptyState } from '@astryxdesign/core/EmptyState';
import { Selector } from '@astryxdesign/core/Selector';
import { Spinner } from '@astryxdesign/core/Spinner';
import { TextInput } from '@astryxdesign/core/TextInput';
import { Page } from '../Page';
import { run } from '../api/client';
import { BillsTable, statusOf, type Listed } from '../bills/BillsTable';

export function Bills({ mine = false }: { mine?: boolean }) {
  const [rows, setRows] = useState<Listed[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [status, setStatus] = useState('all');
  const [sort, setSort] = useState('newest');

  useEffect(() => {
    let active = true;
    setRows(null); setError(null);
    run<{ bills: Listed[] }>('bill.list', mine ? { mine: true } : { limit: 200 })
      .then(r => { if (active) setRows(r.result.bills); })
      .catch(e => { if (active) setError(e.message); });
    return () => { active = false; };
  }, [mine]);

  const filtered = useMemo(() => {
    const needle = search.trim().toLocaleLowerCase();
    const list = (rows ?? []).filter(row => {
      const s = statusOf(row);
      const matches = status === 'all' || (status === 'open' ? !['paid', 'rejected', 'void'].includes(s) : s === status);
      return matches && `${row.bill.vendor_name} ${row.bill.invoice_number}`.toLocaleLowerCase().includes(needle);
    });
    if (sort === 'amount') list.sort((a, b) => b.bill.amount_cents - a.bill.amount_cents);
    if (sort === 'due') list.sort((a, b) => (a.bill.due_date ?? '9999').localeCompare(b.bill.due_date ?? '9999'));
    return list;
  }, [rows, search, status, sort]);

  return <Page title={mine ? 'Waiting on you' : 'All bills'} eyebrow="Accounts payable"
    description="Every invoice, from arrival to payment."
    actions={<Button label="Add invoices" href="/inbox" variant="primary" size="sm" />}>
    <div className="payvy-toolbar">
      <TextInput label="Search bills" isLabelHidden type="search" placeholder="Search vendor or invoice…" value={search} onChange={setSearch} width="100%" />
      <Selector label="Bill status" isLabelHidden value={status} onChange={setStatus} width={190} options={[
        { value: 'all', label: 'All statuses' }, { value: 'open', label: 'Open bills' },
        { value: 'awaiting_approval', label: 'Needs approval' }, { value: 'awaiting_release', label: 'Ready to pay' },
        { value: 'held', label: 'On hold' }, { value: 'paid', label: 'Paid' },
      ]} />
      <Selector label="Sort bills" isLabelHidden value={sort} onChange={setSort} width={180} options={[
        { value: 'newest', label: 'Newest first' }, { value: 'due', label: 'Due date' }, { value: 'amount', label: 'Largest amount' },
      ]} />
    </div>
    {error && <Banner status="error" title="Couldn't load bills" description={error} />}
    {!rows && !error && <Spinner label="Loading bills" />}
    {rows && <>
      <div className="payvy-list-caption"><span>{filtered.length} {filtered.length === 1 ? 'bill' : 'bills'}{search || status !== 'all' ? ` of ${rows.length}` : ''}</span>
        {rows.length === 200 && <span>Most recent 200 bills. Search and filters apply to these bills.</span>}</div>
      {!rows.length ? <EmptyState title="No bills yet" description="Add an invoice to get started. Bills also arrive by email." />
        : !filtered.length ? <EmptyState title="No matching bills" description="Try another vendor, invoice number, or status." />
        : <BillsTable bills={filtered} />}
    </>}
  </Page>;
}
