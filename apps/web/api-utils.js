// Small helpers shared by dashboard.html and find-a-buddy.html. A plain
// script (not a module), loaded with a <script src=...> tag before each
// page's own inline script, same as pledge-options.js.

// Every API route returns JSON, even its errors, except when a request
// never reaches Express at all (wrong path, server down, Caddy/Cloudflare
// returning its own error page). res.json() throws on those instead of
// giving back something we can show, so callers use this instead of
// calling res.json() directly.
async function parseJsonResponse(res) {
  const contentType = res.headers.get('content-type') || '';
  if (!contentType.includes('application/json')) {
    throw new Error(`Unexpected response (${res.status}). Try refreshing the page.`);
  }
  return res.json();
}

function formatBytes(bytes) {
  const gb = bytes / (1024 ** 3);
  return gb >= 1 ? `${gb.toFixed(0)} GB` : `${(bytes / (1024 ** 2)).toFixed(0)} MB`;
}

function formatDate(iso) {
  return new Date(iso).toLocaleDateString();
}

// Shared by every place an API call can come back blocked by the free
// storage allowance (invite creation, direct redeem, marketplace redeem)
// shows the server's explanation plus a jump straight to the Billing
// card instead of leaving the person to go find it. billingHref lets a
// page outside dashboard.html (e.g. find-a-buddy.html) point back at
// dashboard.html#billing instead of just #billing on itself.
function showBlockedError(errorEl, data, fallback, billingHref = '#billing') {
  errorEl.textContent = '';
  errorEl.appendChild(document.createTextNode(data.error || fallback));
  if (data.requires_subscription) {
    const link = document.createElement('a');
    link.href = billingHref;
    link.textContent = 'Go to Billing';
    link.style.marginLeft = '0.5rem';
    errorEl.appendChild(link);
  }
  if (data.email_unverified) {
    const link = document.createElement('a');
    link.href = billingHref.includes('#') ? billingHref.replace(/#.*/, '#verify-banner') : '#verify-banner';
    link.textContent = 'Resend verification email';
    link.style.marginLeft = '0.5rem';
    errorEl.appendChild(link);
  }
}

// Redirects to login on a 401 rather than leaving the caller to handle
// it, every page that calls this is already behind a login wall.
async function loadMe() {
  const res = await fetch('/api/me', { credentials: 'same-origin' });
  if (res.status === 401) {
    window.location.href = '/login.html';
    return null;
  }
  return parseJsonResponse(res);
}
