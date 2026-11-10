// Stand-ins for Mercury and Rho, so the bank settings page connects to
// something real over HTTP. The server reaches them only through sandbox
// connections (PAYVY_MERCURY_SANDBOX_URL, PAYVY_RHO_SANDBOX_URL).
// A token of "bad" is refused, like an expired or wrong token.
import http from 'node:http';

const MERCURY_ACCOUNTS = [
  { id: 'merc-ops', name: 'Mercury Checking ••4410', status: 'active', availableBalance: 52_000.25, canSendRealTimePayments: false },
  { id: 'merc-payroll', name: 'Mercury Payroll ••7781', status: 'active', availableBalance: 1_250, canSendRealTimePayments: false },
];

const RHO_ACCOUNTS = [
  { id: 'rho-cash', account_name: 'Cash (Checking)', balance: { amount: 11_726_353, currency: 'USD' } },
  { id: 'rho-treasury', account_name: 'Treasury', balance: { amount: 0, currency: 'USD' } },
];

const RHO_TRANSACTIONS = [
  {
    id: 'txn-1', account_id: 'rho-cash', account_name: 'Cash (Checking)', transaction_type: 'check_payment',
    status: 'settled', amount: { amount: -7000, currency: 'USD' }, counterparty_name: 'Green Grounds Landscaping',
    initiated_at: '2026-10-14T08:00:48Z', posted_at: '2026-10-15T08:00:00Z',
  },
];

function send(res: http.ServerResponse, status: number, body: unknown) {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(body));
}

export function startFakeBank(port: number): Promise<http.Server> {
  const server = http.createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://bank');
    if (req.headers.authorization === 'Bearer bad') {
      return send(res, 401, { title: 'Unauthorized', detail: 'token is not valid', errors: [{ message: 'token is not valid' }] });
    }
    if (url.pathname === '/mercury/accounts') return send(res, 200, { accounts: MERCURY_ACCOUNTS });
    if (url.pathname === '/mercury/recipients') return send(res, 200, { recipients: [] });
    if (url.pathname === '/rho/accounts') return send(res, 200, { accounts: RHO_ACCOUNTS, page: { next_page_token: null } });
    if (url.pathname === '/rho/transactions') {
      return send(res, 200, { transactions: RHO_TRANSACTIONS, page: { next_page_token: null } });
    }
    send(res, 404, { title: 'Not found' });
  });
  return new Promise((resolve) => server.listen(port, '127.0.0.1', () => resolve(server)));
}
