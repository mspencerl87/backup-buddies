// Small in-memory rate limiter for the endpoints people (or bots) can hammer:
// login, signup, password reset, invite redemption. No dependency and no
// shared store — there's one API process, and a deploy/restart resetting the
// counters is harmless.
//
// Fixed windows per key: the first request for a key starts a window of
// `windowMs`, and up to `max` requests are allowed in it. Over that, the
// request gets a 429 with a Retry-After header and never reaches the route.
//
// Device clients (/devices/*) and relay-1 (/relay/access) are deliberately
// never limited here — they poll constantly and come from few IPs.

const buckets = new Map(); // `${name}:${key}` -> { count, resetAt }

// Drop expired windows once a minute so the map can't grow without bound.
setInterval(() => {
  const now = Date.now();
  for (const [key, bucket] of buckets) {
    if (bucket.resetAt <= now) buckets.delete(key);
  }
}, 60 * 1000).unref();

// The real visitor IP. Requests arrive Cloudflare -> Caddy -> API, so the
// socket address is always Caddy's container and Caddy's X-Forwarded-For is
// a Cloudflare edge IP — neither identifies the visitor.
//
// Caddy works it out and sends it as X-Client-IP (deploy/app/Caddyfile): it
// takes CF-Connecting-IP only from requests that really come from
// Cloudflare's ranges, and the socket address otherwise, overwriting
// whatever the caller sent. Reading CF-Connecting-IP here instead used to
// let anyone who hit app-1 directly (bypassing Cloudflare) set it to
// anything and dodge per-IP limits. The API's port isn't published outside
// Docker, so X-Client-IP can only come from Caddy.
export function clientIp(req) {
  const fromCaddy = req.get('x-client-ip');
  if (fromCaddy && fromCaddy.length <= 45) return fromCaddy.trim();
  // A Caddy still running its old config (it only picks up a changed
  // Caddyfile on reload) doesn't send X-Client-IP; falling straight to
  // X-Forwarded-For would key every visitor by a Cloudflare edge IP and
  // lump strangers into one bucket. Forgeable, as before, but only until
  // the reload.
  const cf = req.get('cf-connecting-ip');
  if (cf && cf.length <= 45) return cf.trim();
  // No Caddy in front (local dev): the last X-Forwarded-For hop, or the
  // socket itself.
  const xff = req.get('x-forwarded-for');
  if (xff) return xff.split(',').pop().trim();
  return req.socket?.remoteAddress || 'unknown';
}

function humanize(ms) {
  const minutes = Math.ceil(ms / 60000);
  return minutes >= 60 ? `${Math.ceil(minutes / 60)} hour(s)` : `${minutes} minute(s)`;
}

// Returns Express middleware. `key(req)` picks what to count by — the
// client IP by default; return null/undefined to skip limiting this request
// (e.g. a login body with no email in it).
export function rateLimit({ name, windowMs, max, key = clientIp, message }) {
  return (req, res, next) => {
    const k = key(req);
    if (k === null || k === undefined || k === '') return next();

    const now = Date.now();
    const id = `${name}:${k}`;
    let bucket = buckets.get(id);
    if (!bucket || bucket.resetAt <= now) {
      bucket = { count: 0, resetAt: now + windowMs };
      buckets.set(id, bucket);
    }
    bucket.count += 1;

    if (bucket.count > max) {
      const retryMs = bucket.resetAt - now;
      res.set('Retry-After', String(Math.ceil(retryMs / 1000)));
      // Logged once per window per key, not on every blocked request, so a
      // bot hammering away doesn't flood the logs.
      if (bucket.count === max + 1) {
        console.warn(`[rate-limit] ${name} limit hit for ${k} (${max} per ${humanize(windowMs)})`);
      }
      return res.status(429).json({
        error: message || `Too many attempts — try again in ${humanize(retryMs)}.`,
      });
    }
    next();
  };
}

// The email a login/reset form submitted, normalized the same way the
// routes do — so "Bob@x.com " and "bob@x.com" share one bucket.
export function bodyEmail(req) {
  const email = req.body?.email;
  return typeof email === 'string' && email.includes('@') ? email.trim().toLowerCase() : null;
}

// For tests: forget everything.
export function _resetRateLimits() {
  buckets.clear();
}
