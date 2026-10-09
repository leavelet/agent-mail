// Public mode in the browser: register with an invite code and send the
// first mail, the settings for notifications and deleting the org, and the
// operator's view. Screenshots go to $AMAIL_M6_SCREENSHOTS (default: the
// screenshots directory).

import fs from 'node:fs';
import path from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { SCREENSHOTS, state } from './env';

test.describe.configure({ mode: 'serial' });

const SHOTS = process.env.AMAIL_M6_SCREENSHOTS ?? SCREENSHOTS;
fs.mkdirSync(SHOTS, { recursive: true });

async function shot(page: Page, name: string) {
  await page.waitForTimeout(250);
  await page.screenshot({ path: path.join(SHOTS, `${name}.png`) });
}

async function pubApi<T = unknown>(token: string, method: string, p: string, body?: unknown): Promise<T> {
  const r = await fetch(`${state().pub.base}/v1${p}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${p} -> ${r.status}: ${text}`);
  return (text ? JSON.parse(text) : undefined) as T;
}

let webToken = '';

test('register with an invite code, then the first mail', async ({ browser }) => {
  const s = state();
  for (const [theme, lang] of [
    ['light', 'en'],
    ['dark', 'zh'],
  ] as const) {
    const ctx = await browser.newContext({ colorScheme: theme, viewport: { width: 1280, height: 860 } });
    const page = await ctx.newPage();
    await page.addInitScript(([l, t]) => {
      localStorage.setItem('amail.lang', l);
      localStorage.setItem('amail.theme', t);
    }, [lang, theme]);
    await page.goto(`${s.pub.base}/`);
    await expect(page.locator(`a[href="${s.basePath}/register"]`)).toBeVisible();
    await page.goto(`${s.pub.base}/register`);
    await page.locator('#r-invite').fill(s.pub.invites[0]);
    await page.locator('#r-org').fill('webco');
    await shot(page, `register-${theme}-${lang}`);
    await ctx.close();
  }

  const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 } });
  const page = await ctx.newPage();
  await page.addInitScript(() => localStorage.setItem('amail.lang', 'en'));
  await page.goto(`${s.pub.base}/register`);
  // A wrong code first.
  await page.locator('#r-invite').fill('inv_000000000000000000000000');
  await page.locator('#r-org').fill('webco');
  await page.locator('form button[type=submit]').click();
  await expect(page.getByRole('alert')).toContainText('not valid');
  await page.locator('#r-invite').fill(s.pub.invites[0]);
  await page.locator('form button[type=submit]').click();
  const tokenEl = page.locator('[data-testid=reg-token]');
  await expect(tokenEl).toHaveText(/^amail_[0-9a-f]{64}$/);
  webToken = (await tokenEl.textContent()) ?? '';
  await expect(page.locator('[data-testid=reg-done]')).toContainText('webco');
  // The next steps: init, then edit the team file, then up.
  await expect(page.locator('.steps li')).toHaveText([/amail login/, /amail node start/, /amail init/, /Edit amail\.toml/, /amail up/]);
  await shot(page, 'register-done-light-en');

  // Sign in with it; a seat (played through the API) answers the first mail.
  await page.getByRole('button', { name: 'Sign in with this token' }).click();
  await page.locator('.shell').waitFor();
  await pubApi(webToken, 'POST', '/seats', { name: 'w1', harness: 'claude', model: 'sonnet' });
  const w1 = (await pubApi<{ token: string }>(webToken, 'POST', '/seats/w1/tokens')).token;
  await page.goto(`${s.pub.base}/#/compose?to=w1`);
  await expect(page.locator('#c-to')).toHaveValue('w1');
  await page.locator('#c-subj').fill('First task');
  await page.locator('#c-body').fill('Write the README.');
  await page.getByRole('button', { name: /^Send/ }).click();
  // w1 has no session yet: the receipt says so.
  await expect(page.getByTestId('send-receipt')).toContainText('w1 offline (no session)');
  await page.getByRole('link', { name: 'Open the thread' }).click();
  await expect(page).toHaveURL(/#\/thread\/\d+/);
  const inbox = await pubApi<{ entries: { message: { id: number; subject: string } }[] }>(w1, 'GET', '/inbox?unread=1');
  expect(inbox.entries.map((e) => e.message.subject)).toEqual(['First task']);
  const id = inbox.entries[0].message.id;
  await pubApi(w1, 'GET', `/messages/${id}`);
  await pubApi(w1, 'POST', '/messages', { to: ['owner'], subject: 'Re: First task', body: 'Done.', in_reply_to: id });
  await page.goto(`${s.pub.base}/#/inbox`);
  await expect(page.locator('.mrow', { hasText: 'Re: First task' })).toBeVisible();
  await ctx.close();
});

test('settings: notifications, token rotation, deleting the org', async ({ browser }) => {
  const s = state();
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 1100 } });
  const page = await ctx.newPage();
  // Headless Chromium denies notifications whatever is granted: stand in
  // for the browser's API (the page asks for permission and remembers the
  // choice; what the system shows is the browser's business).
  await page.addInitScript(() => {
    localStorage.setItem('amail.lang', 'en');
    class Granted {
      static permission = 'default';
      static async requestPermission() {
        Granted.permission = 'granted';
        return 'granted';
      }
      constructor(title: string) {
        (window as unknown as { shown: string[] }).shown ??= [];
        (window as unknown as { shown: string[] }).shown.push(title);
      }
      close() {}
    }
    Object.defineProperty(window, 'Notification', { value: Granted, configurable: true });
  });
  await page.goto(`${s.pub.base}/`);
  await page.locator('#token').fill(webToken);
  await page.locator('form button[type=submit]').click();
  await page.locator('.shell').waitFor();
  await page.goto(`${s.pub.base}/#/admin/settings`);
  const n = page.locator('[data-testid=notify-settings]');
  await n.locator('input[type=checkbox]').check();
  await expect(n).toContainText('Notifications are on');
  expect(await page.evaluate(() => localStorage.getItem('amail.notify'))).toBe('on');
  await n.scrollIntoViewIfNeeded();
  await shot(page, 'notification-settings-light-en');

  // Rotation keeps this browser signed in.
  await page.getByRole('button', { name: 'Rotate my token' }).click();
  const rotated = (await page.locator('[data-testid=rotated-token]').textContent()) ?? '';
  expect(rotated).toMatch(/^amail_/);
  expect((await fetch(`${s.pub.base}/v1/whoami`, { headers: { Authorization: `Bearer ${webToken}` } })).status).toBe(401);
  webToken = rotated;
  await page.reload();
  await page.locator('.shell').waitFor();

  // Deleting needs the org's name.
  await page.goto(`${s.pub.base}/#/admin/settings`);
  const d = page.locator('[data-testid=delete-org]');
  const button = d.getByRole('button', { name: 'Delete the org and all its data' });
  await expect(button).toBeDisabled();
  await d.locator('input').fill('webco');
  await expect(button).toBeEnabled();
  await d.scrollIntoViewIfNeeded();
  await shot(page, 'delete-org-confirm-light-en');
  await button.click();
  await expect(page.locator('#token')).toBeVisible();
  expect((await fetch(`${s.pub.base}/v1/whoami`, { headers: { Authorization: `Bearer ${webToken}` } })).status).toBe(401);
  await ctx.close();
});

test('the operator view', async ({ browser }) => {
  const s = state();
  // Two orgs with some load.
  for (const [i, org] of [
    [1, 'alpha'],
    [2, 'bravo'],
  ] as const) {
    const r = await fetch(`${s.pub.base}/v1/register`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ invite: s.pub.invites[i], org }),
    });
    expect(r.status).toBe(201);
    const token = ((await r.json()) as { token: string }).token;
    for (let k = 0; k < i * 2; k++) await pubApi(token, 'POST', '/seats', { name: `w${k}` });
    for (let k = 0; k < i * 3; k++) await pubApi(token, 'POST', '/messages', { to: ['w0'], subject: `secret subject ${k}`, body: 'secret body' });
  }
  for (const [theme, lang] of [
    ['light', 'en'],
    ['dark', 'zh'],
  ] as const) {
    const ctx = await browser.newContext({ colorScheme: theme, viewport: { width: 1280, height: 720 } });
    const page = await ctx.newPage();
    await page.addInitScript(([l, t]) => {
      localStorage.setItem('amail.lang', l);
      localStorage.setItem('amail.theme', t);
    }, [lang, theme]);
    await page.goto(`${s.pub.base}/admin`);
    await page.locator('#token').fill(s.pub.opToken);
    await page.locator('form button[type=submit]').click();
    const table = page.locator('[data-testid=op-orgs]');
    await expect(table.locator('tr[data-org=bravo]')).toBeVisible();
    // Busiest first; no mail content anywhere.
    await expect(table.locator('tbody tr').first()).toHaveAttribute('data-org', 'bravo');
    expect(await page.content()).not.toContain('secret');
    await shot(page, `operator-${theme}-${lang}`);
    await ctx.close();
  }

  const ctx = await browser.newContext({ viewport: { width: 1280, height: 720 } });
  const page = await ctx.newPage();
  await page.addInitScript(() => localStorage.setItem('amail.lang', 'en'));
  await page.goto(`${s.pub.base}/admin`);
  await page.locator('#token').fill(s.pub.opToken);
  await page.locator('form button[type=submit]').click();
  const row = page.locator('tr[data-org=alpha]');
  page.once('dialog', (d) => void d.accept());
  await row.getByRole('button', { name: 'Suspend' }).click();
  await expect(row).toContainText('suspended');
  await row.getByRole('button', { name: 'Resume' }).click();
  await expect(row).toContainText('active');
  await row.getByRole('button', { name: 'Delete' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.locator('input').fill('alpha');
  await shot(page, 'operator-delete-confirm-light-en');
  await dialog.getByRole('button', { name: 'Delete' }).click();
  await expect(page.locator('tr[data-org=alpha]')).toHaveCount(0);
  await ctx.close();
});
