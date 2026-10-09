// The URL path prefix the app is served under (`amail serve --base-path
// /amail`, or a reverse proxy's location), always ending in `/`: `/` at the
// root, `/amail/` under a prefix. The server writes it into the page as
// <meta name="amail-base" content="/amail/">; without that tag (the Vite dev
// server) it is taken from the page's path. API calls, event streams and
// links to the app's pages are built from it.

/** The app's pages besides the main one, as paths below the prefix. */
const PAGES = ['register', 'admin'];

function valid(p: string | null | undefined): p is string {
  return !!p && p.startsWith('/') && p.endsWith('/') && !p.startsWith('//');
}

/** The prefix from the meta tag's value, else from the page's path. */
export function resolveBase(meta: string | null | undefined, pathname: string): string {
  if (valid(meta)) return meta;
  const parts = pathname.split('/');
  // `/amail/register` -> `/amail/`; `/amail/` -> `/amail/`; `/` -> `/`.
  const last = parts.pop() ?? '';
  if (last !== '' && !PAGES.includes(last)) parts.push(last);
  const p = `${parts.join('/')}/`;
  return valid(p) ? p : '/';
}

function current(): string {
  const doc = globalThis.document;
  const meta = doc?.querySelector?.('meta[name="amail-base"]')?.getAttribute('content');
  return resolveBase(meta, globalThis.location?.pathname ?? '/');
}

export const BASE = current();

/** An API path (`/v1/...`) as a URL below the prefix. */
export function apiUrl(path: string, base = BASE): string {
  return base + path.replace(/^\//, '');
}

/** One of the app's pages: '' (the main page), 'register' or 'admin'. */
export function pageUrl(page: '' | 'register' | 'admin', base = BASE): string {
  return base + page;
}

/** Which of the app's pages is open: '', 'register' or 'admin'. */
export function currentPage(pathname = globalThis.location?.pathname ?? '/', base = BASE): string {
  const rest = pathname.startsWith(base) ? pathname.slice(base.length) : pathname.replace(/^\//, '');
  return PAGES.includes(rest) ? rest : '';
}

/** The server's URL for the CLI and nodes (`AMAIL_URL`): the page's origin
 * and the prefix, without a trailing slash. */
export function serverUrl(origin = globalThis.location?.origin ?? '', base = BASE): string {
  return origin + base.replace(/\/$/, '');
}
