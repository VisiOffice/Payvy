// Cloudflare Email Worker: hands each email sent to the inbound domain to
// Payvy, unchanged. It only signs and forwards; all parsing happens in the
// server (crates/payvy-domain/src/email.rs), where it is tested.
//
// Setup: docs/email-intake.md.

export interface Env {
  /** e.g. https://payvy.example.com — no trailing slash. */
  PAYVY_URL: string;
  /** The same value as the server's PAYVY_INBOUND_SECRET. */
  PAYVY_INBOUND_SECRET: string;
  /**
   * Optional. A verified Email Routing destination that gets the message if
   * Payvy can't be reached after retries, so a bill is never lost.
   */
  FALLBACK_TO?: string;
}

const MAX_BYTES = 25 * 1024 * 1024;
const ATTEMPTS = 3;

export default {
  async email(message: ForwardableEmailMessage, env: Env): Promise<void> {
    if (message.rawSize > MAX_BYTES) {
      message.setReject("Message too large: the limit is 25 MB.");
      return;
    }
    const raw = new Uint8Array(await new Response(message.raw).arrayBuffer());
    const recipient = message.to;

    let lastError = "";
    for (let attempt = 1; attempt <= ATTEMPTS; attempt++) {
      try {
        const res = await post(env, recipient, raw);
        if (res.ok) {
          const body = (await res.json().catch(() => ({}))) as { status?: string; message?: string };
          if (body.status === "ignored") {
            // Not an address Payvy issued (a typo, or one that was replaced).
            message.setReject("No such mailbox.");
          } else if (body.status === "rejected") {
            console.warn(`payvy could not read the message from ${message.from}: ${body.message}`);
          }
          return;
        }
        lastError = `HTTP ${res.status}`;
        if (res.status < 500) break; // a signing or setup problem: retrying won't help
      } catch (e) {
        lastError = String(e);
      }
      if (attempt < ATTEMPTS) await sleep(attempt * 2000);
    }

    console.error(`payvy unreachable for mail to ${recipient}: ${lastError}`);
    if (env.FALLBACK_TO) {
      await message.forward(env.FALLBACK_TO);
      return;
    }
    // Let Cloudflare report the failure to the sending server.
    throw new Error(`payvy unreachable: ${lastError}`);
  },
};

async function post(env: Env, recipient: string, raw: Uint8Array): Promise<Response> {
  const timestamp = Math.floor(Date.now() / 1000).toString();
  const digest = hex(await crypto.subtle.digest("SHA-256", raw));
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(env.PAYVY_INBOUND_SECRET),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const mac = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(`${timestamp}.${recipient}.${digest}`));
  return fetch(`${env.PAYVY_URL}/webhooks/email`, {
    method: "POST",
    headers: {
      "content-type": "message/rfc822",
      "x-payvy-timestamp": timestamp,
      "x-payvy-recipient": recipient,
      "x-payvy-signature": hex(mac),
    },
    body: raw,
  });
}

function hex(buf: ArrayBuffer): string {
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
