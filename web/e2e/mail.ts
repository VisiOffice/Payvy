// Sign-in links. With no mail sender configured the server writes each
// message to its log; the tests read links from there, so signing in runs
// the same code a person's email link does.
import fs from 'node:fs';

const LINK = /(http:\/\/[^\s"]+\/sign-in#token=[0-9a-f]{64})/g;

/** Where the log ends now: links after this point are new. */
export function logMark(log: string): number {
  return fs.existsSync(log) ? fs.statSync(log).size : 0;
}

/** The first sign-in link written after `mark`, waiting for the background send. */
export async function linkAfter(log: string, mark: number, timeoutMs = 10_000): Promise<string> {
  const until = Date.now() + timeoutMs;
  for (;;) {
    const text = fs.readFileSync(log, 'latin1').slice(mark);
    const found = [...text.matchAll(LINK)][0]?.[1];
    if (found) return found;
    if (Date.now() > until) throw new Error('no sign-in link reached the server log');
    await new Promise((r) => setTimeout(r, 100));
  }
}
