# Email intake

Each organization gets an address like `k7f3q9x2m4p8@in.example.com`. Vendors send bills to it,
or you forward them; PDF and image attachments land in the document inbox, including those of a
forwarded email. A `+tag` is allowed (`k7f3q9x2m4p8+fuel@in.example.com`) and ignored.

Payvy doesn't run a mail server. Mail reaches it through a front end that posts each raw message
to `POST /webhooks/email`, signed with a shared secret. The supported front end is
[Cloudflare Email Routing](https://developers.cloudflare.com/email-routing/) with the small
worker in [`workers/email-inbound`](../workers/email-inbound). It is free, and it is what hosted
Payvy uses.

## Self-hosting with Cloudflare

You need a domain on Cloudflare (the free plan is enough). Use a subdomain for inbound mail, such
as `in.example.com`, so it never touches the mail for your main domain.

1. **Pick a secret** of at least 32 characters, e.g. `openssl rand -hex 32`. Keep it in your
   password manager; the server and the worker both need it.
2. **Configure the server**: set `PAYVY_INBOUND_DOMAIN=in.example.com` and
   `PAYVY_INBOUND_SECRET=<secret>`, and restart. `/webhooks/email` must be reachable from the
   internet (directly or through a Cloudflare Tunnel).
3. **Deploy the worker**:
   ```bash
   cd workers/email-inbound
   npm install
   # set PAYVY_URL in wrangler.toml to your server's public URL
   npx wrangler secret put PAYVY_INBOUND_SECRET
   npx wrangler deploy
   ```
4. **Turn on Email Routing** for the domain in the Cloudflare dashboard, add `in.example.com` as
   a subdomain, and let Cloudflare create its MX and SPF records.
5. **Route everything to the worker**: in Email Routing's routing rules, set the catch-all
   address for `in.example.com` to *Send to a Worker* → `payvy-email-inbound`.
6. **Create your address**: a person opens Settings → Bill inbox and clicks *Create address*
   (or runs `email.new_address` over REST or MCP). `email.address`
   shows it afterwards.

Send yourself a test bill and check `email.list` and `document.list`.

### What the worker does

- Retries a failed delivery twice, a few seconds apart.
- If Payvy is still unreachable, forwards the message to `FALLBACK_TO` when you've set one (it
  must be a verified Email Routing destination), so nothing is lost. Otherwise it fails and
  Cloudflare reports the failure to the sending server.
- Bounces mail to addresses Payvy didn't issue, and messages over 25 MB.

## Without Cloudflare

Anything that can run a script per message (Postfix with a pipe transport, a Mailgun or SES
inbound hook, a small IMAP poller) can deliver instead. Post the raw message:

```text
POST /webhooks/email
Content-Type: message/rfc822
X-Payvy-Timestamp: <unix seconds, within 5 minutes of the server's clock>
X-Payvy-Recipient: <the envelope recipient>
X-Payvy-Signature: hex(HMAC-SHA256(secret, "{timestamp}.{recipient}.{hex(sha256(body))}"))

<the raw RFC 5322 message>
```

From a shell:

```bash
ts=$(date +%s); to=k7f3q9x2m4p8@in.example.com
digest=$(sha256sum < bill.eml | cut -d' ' -f1)
sig=$(printf '%s' "$ts.$to.$digest" | openssl dgst -sha256 -hmac "$PAYVY_INBOUND_SECRET" -r | cut -d' ' -f1)
curl -sS https://payvy.example.com/webhooks/email -H "content-type: message/rfc822" \
  -H "x-payvy-timestamp: $ts" -H "x-payvy-recipient: $to" -H "x-payvy-signature: $sig" \
  --data-binary @bill.eml
```

The response says whether to retry. Any 2xx means stop: `imported`, `duplicate` (already here,
caught by Message-ID or exact bytes), `ignored` (an address no organization has) or `rejected`
(not a readable email). A 5xx means try again later. A 401 means the signature, recipient or
clock is wrong.

An agent can also import a saved `.eml` directly with the `email.import` command.

## What gets kept

- The original message, in your document storage at `orgs/{org}/emails/{sha256}.eml`
  (`email.get` gives a download link).
- Every PDF and TIFF, and images sent as attachments. A file's type comes from its bytes, not
  its name.
- Not kept, but listed on the email with a reason: small inline images (signature logos), files
  that aren't PDFs or images, files already in the inbox (with a pointer to the existing
  document), and emails forwarded inside forwarded emails.
- The receiving server's `Authentication-Results` (SPF, DKIM, DMARC), recorded for later use.

## Outbound mail

Not built yet. When Payvy starts sending (approval notifications first), self-hosters will use
[Cloudflare Email Service](https://developers.cloudflare.com/email-service/) on the same domain:
a REST call from the server with an API token. It is in beta and needs the Workers Paid plan;
inbound Email Routing stays free. Hosted Payvy sends through Amazon SES. Both sit behind one
optional setting, and without it Payvy simply doesn't send.
