// The Sent list shows delivery states; a seat at the trust prompt reads the
// same in its row, its panel and the header; the sidebar highlights the
// folder a thread was opened from (or Sent/Inbox by sender); deleting a
// thread asks for it no more; a turn-failure notice leaves "needs you" once
// the seat's turns work again. Screenshots go to $AMAIL_FIX_SCREENSHOTS
// (default: the screenshots directory), named web-*.

import fs from 'node:fs';
import path from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { api, login, SCREENSHOTS, state } from './env';

test.describe.configure({ mode: 'serial' });

const SHOTS = process.env.AMAIL_FIX_SCREENSHOTS ?? SCREENSHOTS;
fs.mkdirSync(SHOTS, { recursive: true });

async function shot(page: Page, name: string) {
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(SHOTS, `${name}.png`) });
}

test('the Sent list shows the delivery state of each message', async ({ page }) => {
  const s = state();
  const sent = await api<{ id: number }>(s.userToken, 'POST', '/messages', { to: ['root', 'be-lead'], cc: ['docs'], subject: 'Sent-state check', body: 'one' });
  // root reads it; be-lead acknowledges it; docs leaves it queued.
  await api(s.seatTokens.root, 'GET', `/messages/${sent.id}`);
  await api(s.seatTokens['be-lead'], 'POST', `/messages/${sent.id}/ack`);
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/inbox?tab=sent`);
  const row = page.locator('.mrow', { hasText: 'Sent-state check' });
  const sum = row.getByTestId('delivery-summary');
  await expect(sum).toHaveText(/1 acked\s*·\s*1 read\s*·\s*1 queued/);
  await expect(sum).toHaveAttribute('title', /Delivery state/);
  // The counts follow the mailbox (docs reads it now).
  await api(s.seatTokens.docs, 'GET', `/messages/${sent.id}`);
  await page.reload();
  await expect(sum).toHaveText(/1 acked\s*·\s*2 read/);
  await shot(page, 'web-sent-list-delivery');

  // The seat panel's Sent tab shows them too.
  await page.goto(`${s.base}/#/org/root`);
  await page.locator('.detail').getByRole('tab', { name: 'Sent' }).click();
  await expect(page.locator('.detail .mrow').first().getByTestId('delivery-summary')).toBeVisible();

  // Chinese (an init script added later runs later).
  await page.addInitScript(() => localStorage.setItem('amail.lang', 'zh'));
  await page.goto(`${s.base}/#/inbox?tab=sent`);
  await page.reload();
  await expect(page.locator('.mrow', { hasText: 'Sent-state check' }).getByTestId('delivery-summary')).toContainText('2 已读');
  await shot(page, 'web-sent-list-delivery-zh');
});

test('a seat at the trust prompt reads the same in its row, its panel and the header', async ({ page }) => {
  const s = state();
  await api(s.userToken, 'POST', '/seats', { name: 'fx-trust', parent: 'root', harness: 'claude', model: 'sonnet' });
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/org`);
  const row = page.locator('.trow[data-seat="fx-trust"]');
  await row.click();
  const panel = page.locator('.detail');
  await expect(panel.getByTestId('seat-state')).toContainText('Offline');
  // The node reports the trust prompt while the panel is open.
  await api(s.nodeToken, 'POST', '/presence', {
    node: 'build-01',
    seats: [{ seat: 'fx-trust', state: 'offline', harness: 'claude', attention: 'trust-prompt', attention_hint: 'Claude Code asks whether to trust /w' }],
  });
  await expect(row.locator('[data-starting]')).toContainText('starting');
  await expect(panel.locator('[data-starting]')).toContainText('starting – waiting for a person');
  await expect(panel.getByTestId('seat-state')).toContainText('Starting: the session waits at a prompt');
  await expect(panel).not.toContainText('no node reports a session');
  await expect(page.getByTestId('top-starting')).toContainText('1', { timeout: 15_000 });
  await shot(page, 'web-seat-trust-prompt-panel');

  // Opened by its URL in a new page: the same.
  await page.goto(`${s.base}/#/org/fx-trust`);
  await page.reload();
  await expect(panel.getByTestId('seat-state')).toContainText('Starting');
  await expect(row.locator('[data-starting]')).toBeVisible();

  await api(s.nodeToken, 'POST', '/presence', { node: 'build-01', seats: [{ seat: 'fx-trust', state: 'offline' }] });
  await api(s.userToken, 'DELETE', '/seats/fx-trust');
});

test('the sidebar highlights the folder a thread was opened from', async ({ page }) => {
  const s = state();
  const mine = await api<{ id: number; thread_id: number }>(s.userToken, 'POST', '/messages', { to: ['root'], subject: 'Folder check: sent', body: 'x' });
  const theirs = await api<{ id: number; thread_id: number }>(s.seatTokens.root, 'POST', '/messages', { to: ['owner'], subject: 'Folder check: received', body: 'y' });
  const between = await api<{ id: number; thread_id: number }>(s.seatTokens['be-lead'], 'POST', '/messages', { to: ['be-1'], subject: 'Folder check: agents', body: 'z' });
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  const active = page.locator('nav a.active');

  // From the Sent tab: Sent.
  await page.goto(`${s.base}/#/inbox?tab=sent`);
  await expect(active).toHaveText(/Sent/);
  await page.locator('.mrow', { hasText: 'Folder check: sent' }).dblclick();
  await expect(page).toHaveURL(/#\/thread\//);
  await expect(page.locator('article').first()).toBeVisible();
  await expect(active).toHaveText(/^Sent$/);
  await shot(page, 'web-thread-sidebar-sent');

  // From the Inbox tab: Inbox.
  await page.goto(`${s.base}/#/inbox`);
  await page.locator('.mrow', { hasText: 'Folder check: received' }).dblclick();
  await expect(page).toHaveURL(/#\/thread\//);
  await expect(active).toHaveText(/Inbox/);

  // By URL, in a fresh page: by the sender of the message shown.
  for (const [m, want] of [
    [mine, /^Sent$/],
    [theirs, /Inbox/],
  ] as const) {
    await page.goto(`${s.base}/#/thread/${m.thread_id}?m=${m.id}`);
    await page.reload();
    await expect(page.locator(`article[data-msg="${m.id}"]`)).toBeVisible();
    await expect(active).toHaveText(want);
  }
  // Mail between two agents: no folder.
  await page.goto(`${s.base}/#/thread/${between.thread_id}`);
  await page.reload();
  await expect(page.locator(`article[data-msg="${between.id}"]`)).toBeVisible();
  await expect(active).toHaveCount(0);
  // From the archive: Archive.
  await page.goto(`${s.base}/#/archive`);
  await page.goto(`${s.base}/#/thread/${between.thread_id}`);
  await expect(active).toHaveText(/Archive/);
});

test('deleting a thread does not ask for it again', async ({ page }) => {
  const s = state();
  const root = await api<{ id: number }>(s.userToken, 'POST', '/messages', { to: ['owner'], subject: 'Delete me', body: 'one' });
  await api(s.userToken, 'POST', '/messages', { to: ['owner'], subject: 'Re: Delete me', body: 'two', in_reply_to: root.id });
  const errors: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error') errors.push(m.text());
  });
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/thread/${root.id}`);
  await expect(page.locator(`article[data-msg="${root.id}"]`)).toBeVisible();
  const after: string[] = [];
  let deleted = false;
  page.on('request', (r) => {
    if (deleted && r.url().includes(`/v1/threads/${root.id}`)) after.push(`${r.method()} ${r.url()}`);
  });
  page.on('response', (r) => {
    if (r.request().method() === 'DELETE' && r.url().includes(`/v1/threads/${root.id}`)) deleted = true;
  });
  page.once('dialog', (d) => void d.accept());
  await page.getByRole('button', { name: 'Delete' }).click();
  await expect(page.getByText('Deleted 2 messages.')).toBeVisible();
  await expect(page.getByRole('link', { name: 'Go to the archive' })).toBeVisible();
  await page.waitForTimeout(1000);
  expect(after).toEqual([]);
  expect(errors.filter((e) => e.includes('404'))).toEqual([]);
  await expect(page.getByText('Thread not found', { exact: false })).toHaveCount(0);
  await shot(page, 'web-thread-deleted');
});

test('a turn-failure notice leaves "needs you" once the seat works again', async ({ page }) => {
  const s = state();
  const failing = (on: boolean) =>
    api(s.nodeToken, 'POST', '/presence', {
      node: 'build-01',
      seats: [
        {
          seat: 'docs',
          state: 'idle',
          harness: 'claude',
          ...(on ? { turn_failed: { reason: 'API Error: 500', at: new Date().toISOString(), count: 3, retrying: false } } : {}),
        },
      ],
    });
  await failing(true);
  await api(s.nodeToken, 'POST', '/node/notices', {
    node: 'build-01',
    notices: [{ kind: 'turn_failures', seat: 'docs', count: 3, harness: 'claude', error: 'API Error: 500' }],
  });
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/needs`);
  const notice = page.locator('.item[data-kind="notice"]', { hasText: 'Turns of seat docs are failing' });
  const seatItem = page.locator('.item[data-kind="seat"]', { hasText: 'docs' });
  await expect(notice).toBeVisible();
  await expect(seatItem).toContainText('turns failing');
  await shot(page, 'web-needs-turn-failure-before');

  // A turn ends normally: the node's report has no failure any more. Both
  // entries go, without a reload.
  await failing(false);
  await expect(seatItem).toHaveCount(0);
  await expect(notice).toHaveCount(0);
  await shot(page, 'web-needs-turn-failure-after');

  // The mail is kept, read, in the inbox.
  await page.goto(`${s.base}/#/inbox`);
  const row = page.locator('.mrow', { hasText: 'Turns of seat docs are failing' });
  await expect(row).toBeVisible();
  await expect(row).not.toHaveClass(/unread/);
});

test('the register page says what a person can delete', async ({ browser }) => {
  const s = state();
  for (const lang of ['en', 'zh'] as const) {
    const ctx = await browser.newContext({ viewport: { width: 1280, height: 860 } });
    const page = await ctx.newPage();
    await page.addInitScript((l) => localStorage.setItem('amail.lang', l), lang);
    await page.goto(`${s.pub.base}/register`);
    const notice = page.locator('section.notice');
    if (lang === 'en') {
      await expect(notice).toContainText('You can delete mail a thread at a time');
      await expect(notice).toContainText('Mail exchanged with orgs that still exist cannot be deleted by either side');
      await expect(notice).not.toContainText('kept until the org is deleted');
    } else {
      await expect(notice).toContainText('你可以按线程删除邮件');
    }
    await notice.scrollIntoViewIfNeeded();
    await shot(page, `web-register-data-${lang}`);
    await ctx.close();
  }
});
