// What the web interface follows live and reports after an action: seats
// created and deleted elsewhere, node connection state, the send receipt,
// seats whose turns fail, trust prompts, notices in "needs you", mark
// read/unread in the inbox, and the delivery legend. Screenshots of these
// states go to $AMAIL_LIVE_SCREENSHOTS (default: the screenshots directory).

import fs from 'node:fs';
import path from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { api, login, pushPresence, SCREENSHOTS, state } from './env';

test.describe.configure({ mode: 'serial' });

const SHOTS = process.env.AMAIL_LIVE_SCREENSHOTS ?? SCREENSHOTS;
fs.mkdirSync(SHOTS, { recursive: true });

async function shot(page: Page, name: string) {
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(SHOTS, `${name}.png`) });
}

const REASON = 'llm-deepseek: no API key for provider route "deepseek-official"';

test('the org page follows seats created and deleted elsewhere', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/org`);
  await expect(page.locator('.trow[data-seat="root"]')).toBeVisible();
  const before = await page.locator('.trow').count();

  // As `amail up` does it: through the API, after the page loaded.
  await api(s.userToken, 'POST', '/seats', { name: 'live-1', parent: 'root', harness: 'dsh', model: 'deepseek-flash' });
  await expect(page.locator('.trow[data-seat="live-1"]')).toBeVisible();
  await expect(page.locator('.trow')).toHaveCount(before + 1);
  await api(s.userToken, 'PATCH', '/seats/live-1', { display_name: 'Live one' });
  await page.locator('.trow[data-seat="live-1"]').click();
  await expect(page.locator('.detail')).toContainText('Live one');
  await api(s.userToken, 'DELETE', '/seats/live-1');
  await expect(page.locator('.trow[data-seat="live-1"]')).toHaveCount(0);
  await expect(page.locator('.detailpane')).toContainText('No seat named live-1');
});

test('sleep and wake stay available while a node is connected', async ({ page }) => {
  const s = state();
  test.skip(!s.node, 'needs the real node');
  await pushPresence();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/org/be-lead`);
  const wake = page.getByRole('button', { name: 'Wake' });
  await expect(wake).toBeEnabled();
  // A presence event after the page loaded (the seat went to sleep) must
  // not turn the node into "not connected".
  await api(s.nodeToken, 'POST', '/presence', { node: 'build-01', seats: [{ seat: 'be-lead', state: 'dormant', harness: 'codex' }] });
  await expect(page.locator('.detail [data-state="dormant"]')).toBeVisible();
  await expect(wake).toBeEnabled();
  await expect(page.locator('.detail')).not.toContainText('not connected');
  await pushPresence();
});

test('compose shows the send receipt', async ({ page }) => {
  const s = state();
  await pushPresence();
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/compose`);
  await page.locator('#c-to').fill('leads, fe-1');
  await page.locator('#c-subj').fill('Interface change: transaction ref ids');
  await page.locator('#c-body').fill('Every transaction gets a `ref` id. Acknowledge when your group has adopted it.');
  await page.getByRole('button', { name: /^Send/ }).click();
  const receipt = page.getByTestId('send-receipt');
  await expect(receipt).toContainText(/Sent message #\d+ to 4 seat\(s\)/);
  await expect(receipt.locator('[data-address="leads"]')).toContainText('3 seat(s) (list in To: wakes 3)');
  await expect(receipt.locator('[data-address="fe-1"]')).toContainText('1 seat(s)');
  await expect(receipt.locator('[data-note="dormant"]')).toContainText('fe-1 asleep; this mail starts it');
  await shot(page, 'web-compose-receipt');

  // The thread opened from the receipt is under Sent (the person sent it).
  await page.getByRole('link', { name: 'Open the thread' }).click();
  await expect(page).toHaveURL(/#\/thread\/\d+/);
  await expect(page.locator('nav a.active')).toHaveText(/^Sent$/);
  const art = page.locator('article').first();
  await art.getByRole('button', { name: 'Delivery' }).click();
  const legend = art.getByTestId('dlv-legend');
  await expect(legend).toContainText('not mean the agent handled it');
  await expect(art.locator('[data-dstate]').first()).toHaveAttribute('title', /.+/);
  await art.getByTestId('delivery').scrollIntoViewIfNeeded();
  await shot(page, 'web-delivery-legend');

  // From the Sent tab, a thread stays under Sent.
  await page.goto(`${s.base}/#/inbox?tab=sent`);
  await page.locator('.mrow', { hasText: 'Interface change' }).dblclick();
  await expect(page).toHaveURL(/#\/thread\//);
  await expect(page.locator('nav a.active')).toHaveText(/^Sent$/);
});

test('a seat whose turns fail is shown as failing', async ({ page }) => {
  const s = state();
  const report = (retrying: boolean, count: number) =>
    api(s.nodeToken, 'POST', '/presence', {
      node: 'build-01',
      seats: [
        {
          seat: 'docs',
          state: 'idle',
          harness: 'dsh',
          turn_failed: { reason: REASON, at: new Date(Date.now() - 60_000).toISOString(), count, retrying },
        },
      ],
    });
  await pushPresence();
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/org`);
  await expect(page.locator('.trow[data-seat="docs"]')).toBeVisible();
  await report(true, 1);
  const row = page.locator('.trow[data-seat="docs"]');
  await expect(row).toHaveClass(/failing/);
  await expect(row).toContainText('turns failing');
  await expect(page.getByTestId('top-turn-failed')).toContainText('1', { timeout: 15_000 });
  await shot(page, 'web-org-failing-seat');

  await report(false, 3);
  await page.goto(`${s.base}/#/org/docs`);
  const box = page.getByTestId('turn-failed');
  await expect(box).toContainText(REASON);
  await expect(box).toContainText('3 failed turn(s) in a row');
  await expect(box).toContainText('stopped showing that mail again');
  await expect(box).toContainText('What to do');
  await shot(page, 'web-seat-turn-failed');

  // The failed turn in a dsh conversation is labelled as an error (the
  // labelling is checked in the unit tests; the mirror needs a node).

  // Needs you: the failing seat, a notice from amail, and message previews.
  await api(s.nodeToken, 'POST', '/node/notices', {
    node: 'build-01',
    notices: [{ kind: 'settings', changes: [{ setting: 'local_policy', old: 'restricted', new: 'open' }] }],
  });
  await page.goto(`${s.base}/#/needs`);
  const seatItem = page.locator('.item[data-kind="seat"]', { hasText: 'docs' });
  await expect(seatItem).toContainText('turns failing');
  await expect(seatItem).toContainText(REASON);
  const notice = page.locator('.item[data-kind="notice"]').first();
  await expect(notice).toContainText('notice');
  await expect(notice).not.toContainText('answer');
  // The preview is the start of the message, not the sender's role.
  const zh = page.locator('.item[data-kind="reply"]', { hasText: '数据库迁移方案' });
  await expect(zh.locator('.preview')).toContainText('建议先迁移只读副本');
  await expect(zh).not.toContainText('Writes the user documentation');
  await seatItem.click();
  await expect(page.locator('.reader')).toContainText('What to do');
  await shot(page, 'web-needs-failing-and-notice');

  // Dark theme example.
  await page.evaluate(() => localStorage.setItem('amail.theme', 'dark'));
  await page.goto(`${s.base}/#/org/docs`);
  await page.reload();
  await expect(page.getByTestId('turn-failed')).toBeVisible();
  await shot(page, 'web-seat-turn-failed-dark');
  await page.evaluate(() => localStorage.setItem('amail.theme', 'light'));

  // The seat recovered: the flag goes away.
  await api(s.nodeToken, 'POST', '/presence', { node: 'build-01', seats: [{ seat: 'docs', state: 'idle', harness: 'dsh' }] });
  await page.goto(`${s.base}/#/org`);
  await expect(page.locator('.trow[data-seat="docs"]')).not.toHaveClass(/failing/);
});

test('a seat at the trust prompt: starting, and what to do', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await api(s.nodeToken, 'POST', '/presence', {
    node: 'build-01',
    seats: [{ seat: 'docs', state: 'offline', harness: 'claude', attention: 'trust-prompt', attention_hint: 'Claude Code asks whether to trust /w' }],
  });
  await page.goto(`${s.base}/#/org/docs`);
  await expect(page.locator('.detail [data-starting]')).toContainText('starting – waiting for a person');
  const help = page.getByTestId('trust-help');
  await expect(help).toContainText('amail attach docs');
  await expect(help).toContainText('Yes, I trust this folder');
  await expect(help).toContainText('Ctrl-o d');
  await expect(help).toContainText('--web-terminal-port');
  await shot(page, 'web-seat-trust-prompt');
  await api(s.nodeToken, 'POST', '/presence', { node: 'build-01', seats: [{ seat: 'docs', state: 'offline' }] });
});

test('opening mail in the inbox marks it read; Mark unread undoes it', async ({ page }) => {
  const s = state();
  const id = (await api<{ id: number }>(s.seatTokens.root, 'POST', '/messages', { to: ['owner'], subject: 'Final report', body: 'All merged into main.\n18 tests pass.' })).id;
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/needs`);
  await expect(page.locator('.item', { hasText: 'Final report' })).toBeVisible();
  await page.goto(`${s.base}/#/inbox`);
  await page.locator('.mrow', { hasText: 'Final report' }).click();
  const reader = page.locator('.reader');
  await expect(reader).toContainText('All merged into main');
  await expect(reader.getByRole('button', { name: /Mark unread/ })).toBeVisible();
  const d = await api<{ delivery: { read_at?: string } }>(s.userToken, 'GET', `/messages/${id}?peek=1`);
  expect(d.delivery.read_at).toBeTruthy();
  await page.goto(`${s.base}/#/needs`);
  await expect(page.locator('.item', { hasText: 'Final report' })).toHaveCount(0);
  await page.goto(`${s.base}/#/inbox`);
  await page.locator('.mrow', { hasText: 'Final report' }).click();
  await reader.getByRole('button', { name: /Mark unread/ }).click();
  await expect(reader.getByRole('button', { name: /Mark read/ })).toBeVisible();
  await page.goto(`${s.base}/#/needs`);
  await expect(page.locator('.item', { hasText: 'Final report' })).toBeVisible();
  // Done with it.
  await api(s.userToken, 'POST', `/messages/${id}/read`);
});

test('admin seats offers dsh', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/admin/seats`);
  const sel = page.locator('form select').first();
  await expect(sel.locator('option')).toHaveText(['—', 'claude', 'codex', 'dsh']);
  await sel.selectOption('dsh');
  await shot(page, 'web-admin-seats-dsh');
});
