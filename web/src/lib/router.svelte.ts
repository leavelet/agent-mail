// Hash routing: `#/path/segments?query`.

export interface Route {
  parts: string[];
  query: URLSearchParams;
}

export function parseHash(hash: string): Route {
  const h = hash.replace(/^#/, '');
  const [path, q = ''] = h.split('?', 2);
  const parts = path
    .split('/')
    .filter(Boolean)
    .map((p) => {
      try {
        return decodeURIComponent(p);
      } catch {
        return p;
      }
    });
  return { parts, query: new URLSearchParams(q) };
}

class Router {
  route = $state<Route>(parseHash(globalThis.location?.hash ?? ''));

  constructor() {
    globalThis.addEventListener?.('hashchange', () => {
      this.route = parseHash(globalThis.location.hash);
    });
  }
}

export const router = new Router();

/** A hash href from path segments and an optional query. */
export function href(parts: (string | number)[], query?: Record<string, string | number | undefined>): string {
  const path = parts.map((p) => encodeURIComponent(String(p))).join('/');
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(query ?? {})) if (v !== undefined && v !== '') q.set(k, String(v));
  const qs = q.toString();
  return `#/${path}${qs ? `?${qs}` : ''}`;
}

export function go(
  parts: (string | number)[],
  query?: Record<string, string | number | undefined>,
  replace = false,
): void {
  const h = href(parts, query);
  if (replace) globalThis.location.replace(h);
  else globalThis.location.hash = h;
}
