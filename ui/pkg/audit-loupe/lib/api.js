// The backend runs in one cluster (by default Rancher's own, `local`). The UI reaches it through
// the Rancher service proxy, so Rancher login and RBAC decide who sees audit data: a user needs
// "get" on services/proxy in the backend's namespace.
//
// When the backend runs elsewhere, the page asks for that cluster once and remembers it in this
// browser.

export const BACKEND = {
  namespace: 'audit-loupe-system',
  service:   'audit-loupe',
  port:      8080,
};

const KEY = 'audit-loupe.backend-cluster';

export function backendCluster() {
  try {
    return window.localStorage.getItem(KEY) || 'local';
  } catch (e) {
    return 'local';
  }
}

export function setBackendCluster(id) {
  try {
    if (id && id !== 'local') {
      window.localStorage.setItem(KEY, id);
    } else {
      window.localStorage.removeItem(KEY);
    }
  } catch (e) {}
}

export function baseUrl(cluster = backendCluster()) {
  const { namespace, service, port } = BACKEND;

  return `/k8s/clusters/${ encodeURIComponent(cluster) }/api/v1/namespaces/${ namespace }/services/http:${ service }:${ port }/proxy`;
}

export async function getJson(path, params = {}) {
  const qs = new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== null && v !== '')).toString();
  const res = await fetch(`${ baseUrl() }${ path }${ qs ? `?${ qs }` : '' }`, {
    credentials: 'same-origin',
    headers:     { Accept: 'application/json' },
  });

  if (!res.ok) {
    const err = new Error(`HTTP ${ res.status }`);

    err.status = res.status;
    throw err;
  }

  return res.json();
}

// Rancher accepts a cookie-authenticated POST only with its CSRF cookie echoed in a header,
// as its own UI does.
function csrf() {
  const m = document.cookie.match(/(?:^|;\s*)CSRF=([^;]+)/);

  return m ? decodeURIComponent(m[1]) : '';
}

export async function postJson(path) {
  const res = await fetch(`${ baseUrl() }${ path }`, {
    method:      'POST',
    credentials: 'same-origin',
    headers:     { Accept: 'application/json', 'x-api-csrf': csrf() },
  });
  const body = await res.json().catch(() => ({}));

  if (!res.ok) {
    const err = new Error(body.error || `HTTP ${ res.status }`);

    err.status = res.status;
    throw err;
  }

  return body;
}
