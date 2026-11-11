// The web app calls the same commands as agents and the MCP server, signed in
// by the session cookie. Inputs are typed from the server's command list
// (commands.gen.ts); results are what each command documents.
import type { CommandInputs, CommandName } from './commands.gen';

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}

/** Every cookie-signed call carries this; the server refuses one without it. */
const CLIENT = { 'x-payvy-client': 'web' };

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    credentials: 'same-origin',
    headers: { ...CLIENT, ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!res.ok) {
    if (res.status === 403) window.dispatchEvent(new Event('payvy:access-changed'));
    const e = data?.error ?? {};
    throw new ApiError(res.status, e.code ?? 'error', e.message ?? res.statusText);
  }
  return data as T;
}

export interface Me {
  roles: import('../permissions').Role[];
  commands: CommandName[];
  person: { actor: string; name: string; email: string };
  organization: { id: string; name: string };
}

export const auth = {
  requestLink: (email: string) =>
    request<{ message: string }>('POST', '/auth/link', { email }),
  redeem: (token: string) => request<Me>('POST', '/auth/session', { token }),
  me: () => request<Me>('GET', '/auth/session'),
  signOut: () => request<null>('DELETE', '/auth/session'),
};

export interface Committed<R> {
  command: string;
  mode: 'commit';
  result: R;
  events: string[];
}

export interface Previewed<R> {
  command: string;
  mode: 'preview';
  result: R;
  events: string[];
  /** Present for confirmed commands: quote it to commit exactly this. */
  confirm_token?: string;
}

/** Runs a command. Confirmed-tier commands need `confirmToken` from a preview. */
export function run<R = unknown, N extends CommandName = CommandName>(
  name: N,
  input: CommandInputs[N],
  confirmToken?: string,
): Promise<Committed<R>> {
  return request<Committed<R>>('POST', `/api/commands/${name}`, {
    input,
    mode: 'commit',
    confirm_token: confirmToken,
  }).then(result => {
    // Keep navigation counts current after writes, including actions on the same page.
    if (result.events.length) window.dispatchEvent(new Event('payvy:changed'));
    return result;
  });
}

/** Shows what a command would do, changing nothing. */
export function preview<R = unknown, N extends CommandName = CommandName>(
  name: N,
  input: CommandInputs[N],
): Promise<Previewed<R>> {
  return request('POST', `/api/commands/${name}`, { input, mode: 'preview' });
}
