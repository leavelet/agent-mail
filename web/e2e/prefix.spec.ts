// The app under a URL path prefix. With AMAIL_E2E_BASE_PATH set, the
// servers of global-setup run with `--base-path` and every spec uses URLs
// with the prefix; at the root these tests check the same things there.
// The last test runs a server behind a small reverse proxy that removes a
// prefix and sends X-Forwarded-Prefix.

import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { api, login, ROOT, state } from './env';

test.describe.configure({ mode: 'serial' });

const BIN = process.env.AMAIL_BIN ?? path.resolve(ROOT, '../target/debug/amail');

/** Every request the page makes, as paths, collected from now on. */
function requests(page: Page): string[] {
  const seen: string[] = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.protocol.startsWith('http')) seen.push(u.pathname);
  });
  return seen;
}

test('the app loads, calls the API and links below the prefix', async ({ page, context }) => {
  const s = state();
  const prefix = s.basePath;
  const seen = requests(page);
  await login(page, s.userToken, { lang: 'en' });
  await expect(page.locator('.trow[data-seat="root"]')).toBeVisible();
  expect(seen.length).toBeGreaterThan(3);
  for (const p of seen) expect(p.startsWith(`${prefix}/`), p).toBe(true);
  await expect.poll(() => seen).toContain(`${prefix}/v1/events`);
  // The session cookie is scoped to the prefix.
  const cookie = (await context.cookies()).find((c) => c.name === 'amail_session');
  expect(cookie?.path).toBe(prefix || '/');

  // A link to an app route given as a path opens that route.
  await page.goto(`${s.base}/org/be-lead`);
  await expect(page).toHaveURL(`${s.base}/#/org/be-lead`);
  await expect(page.locator('.trow[data-seat="be-lead"]')).toBeVisible();
  // Mail sent meanwhile shows up live (the event stream below the prefix).
  await page.goto(`${s.base}/#/inbox`);
  await api(s.seatTokens.docs, 'POST', '/messages', { to: ['owner'], subject: 'Prefix check: live', body: 'x' });
  await expect(page.locator('.mrow', { hasText: 'Prefix check: live' })).toBeVisible();

  if (prefix) {
    // The prefix without its trailing slash goes to the app.
    const origin = new URL(s.base).origin;
    await page.goto(`${origin}${prefix}`);
    await expect(page).toHaveURL(`${origin}${prefix}/`);
    await expect(page.locator('.shell')).toBeVisible();
    // Nothing is served at the root but the health check.
    const root = await fetch(`${origin}/`);
    expect(root.status).toBe(404);
    expect((await fetch(`${origin}/healthz`)).ok).toBe(true);
  }
});

test('registration and operator pages under the prefix', async ({ page }) => {
  const s = state();
  const prefix = s.basePath;
  const pubPath = new URL(s.pub.base).pathname.replace(/\/$/, '');
  expect(pubPath).toBe(prefix);
  await page.goto(`${s.pub.base}/`);
  const reg = page.locator('a.reg');
  await expect(reg).toHaveAttribute('href', `${prefix}/register`);
  await reg.click();
  await expect(page).toHaveURL(`${s.pub.base}/register`);
  await expect(page.locator('form')).toBeVisible();
  const info = (await (await fetch(`${s.pub.base}/v1/info`)).json()) as { base_path: string };
  expect(info.base_path).toBe(prefix || '/');
  // The operator's page signs in and out below the prefix.
  await page.goto(`${s.pub.base}/admin`);
  await page.locator('#token').fill(s.pub.opToken);
  await page.locator('form button[type=submit]').click();
  await expect(page.getByRole('button', { name: /Sign out|退出/ })).toBeVisible();
  await page.getByRole('button', { name: /Sign out|退出/ }).click();
  await expect(page).toHaveURL(`${s.pub.base}/`);
});

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.listen(0, '127.0.0.1', () => {
      const port = (srv.address() as net.AddressInfo).port;
      srv.close(() => resolve(port));
    });
    srv.on('error', reject);
  });
}

/** A reverse proxy like nginx's `location /px/ { proxy_pass http://up/; }`
 * with the X-Forwarded-* headers; streams responses (event streams). */
function stripProxy(listen: number, upstream: number, strip: string): http.Server {
  return http.createServer((req, res) => {
    const url = req.url ?? '/';
    if (!url.startsWith(`${strip}/`)) {
      res.writeHead(404).end('not proxied');
      return;
    }
    const up = http.request(
      {
        host: '127.0.0.1',
        port: upstream,
        method: req.method,
        path: url.slice(strip.length),
        headers: {
          ...req.headers,
          host: `127.0.0.1:${upstream}`,
          'x-forwarded-host': req.headers.host ?? '',
          'x-forwarded-proto': 'http',
          'x-forwarded-for': req.socket.remoteAddress ?? '',
          'x-forwarded-prefix': strip,
        },
      },
      (r) => {
        res.writeHead(r.statusCode ?? 502, r.headers);
        r.pipe(res);
      },
    );
    up.on('error', () => res.destroy());
    req.pipe(up);
  }).listen(listen, '127.0.0.1');
}

test('behind a proxy that removes the prefix (X-Forwarded-Prefix)', async ({ page, context }) => {
  const s = state();
  const prefix = s.basePath;
  const dir = path.join(s.dir, 'proxied');
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  const db = `sqlite://${path.join(dir, 'amail.db')}`;
  const created = spawnSync(BIN, ['--json', 'admin', 'create-org', 'proxied', '--db', db], { encoding: 'utf8' });
  expect(created.status, created.stderr).toBe(0);
  const token = (JSON.parse(created.stdout) as { token: string }).token;
  const port = await freePort();
  const args = ['serve', '--db', db, '--listen', `127.0.0.1:${port}`, '--trusted-proxy', '127.0.0.1'];
  if (prefix) args.push('--base-path', prefix);
  const log = fs.openSync(path.join(dir, 'serve.log'), 'a');
  const procs: ChildProcess[] = [spawn(BIN, args, { stdio: ['ignore', log, log] })];
  const proxyPort = await freePort();
  const proxy = stripProxy(proxyPort, port, '/px');
  try {
    for (let i = 0; i < 100; i++) {
      try {
        if ((await fetch(`http://127.0.0.1:${port}/healthz`)).ok) break;
      } catch {
        // not yet
      }
      await new Promise((r) => setTimeout(r, 100));
    }
    // Reached as localhost (not 127.0.0.1): another host than the upstream,
    // as with a real proxy.
    const base = `http://localhost:${proxyPort}/px${prefix}`;
    const seen = requests(page);
    await page.goto(`${base}/`);
    await page.locator('#token').fill(token);
    await page.locator('form button[type=submit]').click();
    await page.locator('.shell').waitFor();
    for (const p of seen) expect(p.startsWith(`/px${prefix}/`), p).toBe(true);
    await expect.poll(() => seen).toContain(`/px${prefix}/v1/events`);
    const cookie = (await context.cookies(base)).find((c) => c.name === 'amail_session');
    expect(cookie?.path).toBe(`/px${prefix}`);

    // Live mail through the proxy.
    const call = async (method: string, p: string, body?: unknown) => {
      const r = await fetch(`${base}/v1${p}`, {
        method,
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      expect(r.ok, `${method} ${p}: ${r.status}`).toBe(true);
      return (await r.json()) as Record<string, unknown>;
    };
    expect((await call('GET', '/info')).base_path).toBe(`/px${prefix}`);
    await call('POST', '/seats', { name: 'w1', harness: 'claude', model: 'sonnet' });
    const seatToken = (await call('POST', '/seats/w1/tokens', { label: 'e2e' })).token as string;
    await page.goto(`${base}/#/inbox`);
    const r = await fetch(`${base}/v1/messages`, {
      method: 'POST',
      headers: { Authorization: `Bearer ${seatToken}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ to: ['owner'], subject: 'Through the proxy', body: 'x' }),
    });
    expect(r.ok).toBe(true);
    await expect(page.locator('.mrow', { hasText: 'Through the proxy' })).toBeVisible();
    // A write from the page (cookie, Origin of the proxy's host).
    await page.goto(`${base}/#/compose?to=w1`);
    await page.locator('#c-subj').fill('From behind the proxy');
    await page.locator('#c-body').fill('y');
    await page.getByRole('button', { name: /^Send/ }).click();
    await expect(page.getByTestId('send-receipt')).toContainText('w1: 1 seat');
    // Deep link.
    await page.goto(`${base}/org/w1`);
    await expect(page).toHaveURL(`${base}/#/org/w1`);
    await expect(page.locator('.trow[data-seat="w1"]')).toBeVisible();
  } finally {
    proxy.close();
    for (const p of procs) p.kill('SIGTERM');
  }
});
