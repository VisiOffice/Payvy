// Static assets are served by Cloudflare. Only API/auth paths enter this Worker.
// Rust remains the sole authority for sessions, commands and payment confirmation.
interface Env { API_ORIGIN: string }

function error(status: number, message: string) {
  return Response.json({ error: { code: 'edge_error', message } }, { status, headers: { 'Cache-Control': 'no-store' } });
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (!/^\/(api|auth)(\/|$)/.test(url.pathname)) return error(404, 'Not found');
    // Preserve the browser's same-origin session boundary, including sign-in.
    const origin = request.headers.get('Origin');
    if (!['GET', 'HEAD'].includes(request.method) && origin && origin !== url.origin) {
      return error(403, 'Cross-origin requests are not allowed');
    }
    let backend: URL;
    try { backend = new URL(env.API_ORIGIN); } catch { return error(503, 'API origin is not configured correctly'); }
    const local = backend.protocol === 'http:' && backend.hostname === '127.0.0.1';
    if ((!local && backend.protocol !== 'https:') || backend.origin === url.origin
      || backend.username || backend.password || backend.pathname !== '/' || backend.search || backend.hash) {
      return error(503, 'API origin is not configured correctly');
    }
    // Assign the origin instead of resolving an untrusted path against it.
    url.protocol = backend.protocol;
    url.host = backend.host;
    const upstream = new Request(url, request);
    upstream.headers.delete('Host');
    upstream.headers.delete('Forwarded');
    upstream.headers.set('X-Forwarded-Host', new URL(request.url).host);
    upstream.headers.set('X-Forwarded-Proto', new URL(request.url).protocol.slice(0, -1));
    try {
      // Stream uploads/downloads and preserve cookies, status and command headers.
      // Never follow redirects with session credentials, cache, or retry a write.
      const response = await fetch(upstream, { redirect: 'manual', cache: 'no-store' });
      const result = new Response(response.body, response);
      result.headers.set('Cache-Control', 'no-store');
      result.headers.set('X-Robots-Tag', 'noindex, nofollow');
      return result;
    } catch {
      return error(502, 'Payvy could not reach the API. Check the current state before retrying an action.');
    }
  },
};
