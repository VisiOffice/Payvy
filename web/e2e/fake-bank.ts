// Stand-ins for Mercury and Rho, so bank settings and recipient invitations
// talk to something real over HTTP. The server reaches them only through
// sandbox connections in debug builds (PAYVY_MERCURY_SANDBOX_URL,
// PAYVY_RHO_SANDBOX_URL). A token of "bad" is refused, like an expired one.
// /mercury/test/* lets a test look inside or move an invitation along.
import http from 'node:http';

/** A Mercury token that reaches two accounts, so Payvy must ask which pays. */
export const TWO_ACCOUNT_TOKEN = 'merc-good';

const MERCURY_ACCOUNTS = [
  { id: 'merc-ops', name: 'Mercury Checking ••4410', status: 'active', availableBalance: 52_000.25 },
  { id: 'merc-payroll', name: 'Mercury Payroll ••7781', status: 'active', availableBalance: 1_250 },
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

/** The recipient an invitation turns into once the vendor completes it. */
const INVITED_RECIPIENT = {
  id: 'recipient-invited', name: 'Invited vendor', status: 'active', emails: ['vendor@example.com'],
  electronicRoutingInfo: { accountNumber: '000123456789', routingNumber: '121000248' },
};

function send(res: http.ServerResponse, status: number, body: unknown) {
  res.writeHead(status, { 'content-type': 'application/json' });
  res.end(JSON.stringify(body));
}

async function json(req: http.IncomingMessage): Promise<Record<string, unknown>> {
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(Buffer.from(chunk));
  return chunks.length ? JSON.parse(Buffer.concat(chunks).toString()) : {};
}

export function startFakeBank(port: number): Promise<http.Server> {
  const invites = new Map<string, Record<string, unknown>>();
  let posts = 0;
  let recipientPosts = 0;
  let rampTokenStatus = 200;
  let rampTokenTtl = 3600;
  let rampTokenRequests = 0;
  let rampWrites = 0;

  const server = http.createServer(async (req, res) => {
    const path = new URL(req.url ?? '/', 'http://bank').pathname;

    if (path === '/ramp/test/config') {
      const config = await json(req);
      if (config.reset) { rampTokenStatus = 200; rampTokenTtl = 3600; rampTokenRequests = 0; rampWrites = 0; }
      if (typeof config.tokenStatus === 'number') rampTokenStatus = config.tokenStatus;
      if (typeof config.tokenTtl === 'number') rampTokenTtl = config.tokenTtl;
      return send(res, 200, { tokenRequests: rampTokenRequests, writes: rampWrites });
    }
    if (path.startsWith('/developer/v1/')) {
      const id = '00000000-0000-4000-8000-000000000001';
      const second = '00000000-0000-4000-8000-000000000002';
      const page = (data: unknown) => ({ data, page: { next: null } });
      if (path === '/developer/v1/token' && req.method === 'POST') {
        rampTokenRequests++;
        if (rampTokenStatus !== 200) return send(res, rampTokenStatus, { private: 'fake-secret-error-must-stay-server-side' });
        return send(res, 200, {
          access_token: 'fake-ramp-access', refresh_token: 'fake-ramp-refresh', expires_in: rampTokenTtl,
          token_type: 'Bearer', scope: 'business:read entities:read bank_accounts:read vendors:read payments:read',
        });
      }
      if (req.method !== 'GET') { rampWrites++; return send(res, 405, { error: 'Ramp writes are forbidden' }); }
      if (req.headers.authorization !== 'Bearer fake-ramp-access') return send(res, 401, {});
      if (path.endsWith('/business')) return send(res, 200, { id, business_name_legal: 'Ramp Test Company', active: true });
      if (path.endsWith('/entities')) return send(res, 200, page([{ id, entity_name: 'US Operations', currency: 'USD', payment_accounts: [{ id, account_name: 'Operating', account_number_last_four: '0042', currency: 'USD' }] }]));
      if (path.endsWith('/bank-accounts')) return send(res, 200, page([{ id, account_name: 'Operating', account_number_last_four: '0042', has_ach_details: true }]));
      if (path === `/developer/v1/vendors/${id}`) return send(res, 200, { id, name: 'Ramp Supplier', is_active: true });
      if (path.endsWith('/vendors')) return send(res, 200, page([{ id, name: 'Ramp Supplier', is_active: true }]));
      if (path.endsWith('/payment-methods')) return send(res, 200, page([
        { id, type: 'ACH', account_number_last_four: '1234', fee: { amount: 0, currency_code: 'USD', minor_unit_conversion_rate: 100 } },
        { id: second, type: 'ACH', account_number_last_four: '5678', fee: { amount: 100, currency_code: 'USD', minor_unit_conversion_rate: 100 } },
      ]));
      return send(res, 404, {});
    }

    // Test controls: how many invitations were sent; complete them all.
    if (path === '/mercury/test/posts') return send(res, 200, { posts, recipientPosts });
    if (path === '/mercury/test/complete') {
      for (const invite of invites.values()) Object.assign(invite, { status: 'completed', recipientId: INVITED_RECIPIENT.id });
      return send(res, 200, {});
    }

    if (req.headers.authorization === 'Bearer bad') {
      return send(res, 401, { title: 'Unauthorized', detail: 'token is not valid', errors: [{ message: 'token is not valid' }] });
    }

    if (path === '/mercury/accounts') {
      const accounts =
        req.headers.authorization === `Bearer ${TWO_ACCOUNT_TOKEN}` ? MERCURY_ACCOUNTS : MERCURY_ACCOUNTS.slice(0, 1);
      return send(res, 200, { accounts });
    }
    if (path === '/mercury/recipients' && req.method === 'POST') {
      const body = await json(req);
      recipientPosts++;
      return send(res, 200, { ...body, id: `recipient-entered-${recipientPosts}`, status: 'active', defaultPaymentMethod: 'ach' });
    }
    if (path === '/mercury/recipients') return send(res, 200, { recipients: [] });
    if (path === '/mercury/recipients/invites' && req.method === 'POST') {
      const body = await json(req);
      const id = `invite-${++posts}`;
      const invite = {
        id, status: 'created', name: body.name, contactEmail: body.contactEmail, paymentMethods: body.paymentMethods,
        requireTaxDocument: body.requireTaxDocument, recipientId: null, createdAt: new Date().toISOString(),
        // A capability link: Payvy must never store or show it.
        onboardingUrl: 'https://mercury.com/secret-invite-capability',
      };
      invites.set(id, invite);
      return send(res, 201, invite);
    }
    if (path.startsWith('/mercury/recipients/invites/')) {
      const invite = invites.get(path.split('/').pop() ?? '');
      return send(res, invite ? 200 : 404, invite ?? {});
    }
    if (path === `/mercury/recipient/${INVITED_RECIPIENT.id}`) return send(res, 200, INVITED_RECIPIENT);

    if (path === '/rho/accounts') return send(res, 200, { accounts: RHO_ACCOUNTS, page: { next_page_token: null } });
    if (path === '/rho/transactions') {
      return send(res, 200, { transactions: RHO_TRANSACTIONS, page: { next_page_token: null } });
    }
    send(res, 404, { title: 'Not found' });
  });
  return new Promise((resolve) => server.listen(port, '127.0.0.1', () => resolve(server)));
}
