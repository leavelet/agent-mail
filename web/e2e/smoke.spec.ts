import fs from 'node:fs';
import { expect, test } from '@playwright/test';
import { api, login, pushPresence, state } from './env';

test.describe.configure({ mode: 'serial' });

test('a seat token cannot sign in', async ({ page }) => {
  const s = state();
  await page.goto(`${s.base}/`);
  await page.locator('#token').fill(s.seatTokens['be-1']);
  await page.locator('form button[type=submit]').click();
  await expect(page.getByRole('alert')).toContainText(/Only user tokens|只有用户令牌/);
  await page.locator('#token').fill('amail_' + '0'.repeat(64));
  await page.locator('form button[type=submit]').click();
  await expect(page.getByRole('alert')).toContainText(/invalid|无效/);
});

test('smoke: org, compose, reply, thread, delivery, search, mirror, needs-you', async ({ page }) => {
  const s = state();
  await pushPresence();
  await login(page, s.userToken, { lang: 'en' });

  // The org tree shows the seats with their states.
  await expect(page.locator('.trow[data-seat="root"]')).toBeVisible();
  await expect(page.locator('.trow[data-seat="be-lead"] [data-state="idle"]')).toBeVisible();
  await expect(page.locator('.trow[data-seat="fe-lead"]')).toContainText('permission prompt');
  await expect(page.locator('.trow[data-seat="be-lead"]')).toContainText('waiting for you');
  // A seat in focus says so, live.
  await api(s.userToken, 'PUT', '/focus?seat=be-1', { for_s: 1800 });
  await expect(page.locator('.trow[data-seat="be-1"]')).toContainText('focus until');
  await api(s.userToken, 'DELETE', '/focus?seat=be-1');

  // Compose to be-lead.
  await page.goto(`${s.base}/#/compose?to=be-lead`);
  await expect(page.locator('#c-to')).toHaveValue('be-lead');
  await page.locator('#c-subj').fill('Use a bounded scan');
  await page.locator('#c-body').fill('Scan the most recent 2,000 messages and say so in the response.');
  await page.getByRole('button', { name: /^Send/ }).click();
  await expect(page.getByTestId('send-receipt')).toContainText('be-lead: 1 seat');
  await page.getByRole('link', { name: 'Open the thread' }).click();
  await expect(page).toHaveURL(/#\/thread\/\d+/);
  const sentId = Number(new URL(page.url()).hash.match(/m=(\d+)/)?.[1]);
  expect(sentId).toBeGreaterThan(0);
  await expect(page.locator(`article[data-msg="${sentId}"]`)).toContainText('Use a bounded scan');

  // be-lead (played through the API) reads it and replies.
  const token = s.seatTokens['be-lead'];
  await api(token, 'GET', `/messages/${sentId}`);
  await api(token, 'POST', '/messages', { to: ['owner'], subject: 'Re: Use a bounded scan', body: 'Done: scan mode is marked in the response.', in_reply_to: sentId });

  // The reply shows up in the inbox (live update, no reload).
  await page.goto(`${s.base}/#/inbox`);
  const row = page.locator('.mrow', { hasText: 'Re: Use a bounded scan' });
  await expect(row).toBeVisible();
  await row.click();
  await expect(page.locator('.reader')).toContainText('Done: scan mode is marked');
  // Open the thread with the keyboard and check the delivery status.
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/#\/thread\//);
  const first = page.locator(`article[data-msg="${sentId}"]`);
  await first.getByRole('button', { name: 'Delivery' }).click();
  await expect(first.locator('[data-testid="delivery"]')).toContainText('be-lead');
  await expect(first.locator('[data-dstate="read"]')).toBeVisible();

  // Chinese search: a query of three characters and a short one.
  await page.goto(`${s.base}/#/archive`);
  await page.locator('input[data-search]').fill('迁移方案');
  await page.keyboard.press('Enter');
  await expect(page.locator('.results[data-mode="scan"] .mrow')).toContainText('数据库迁移方案');
  await page.locator('input[data-search]').fill('迁移');
  await page.keyboard.press('Enter');
  await expect(page.locator('.results[data-mode="scan"]')).toBeVisible();
  await expect(page.locator('.results .mrow')).toContainText('数据库迁移方案');
  // The org's few messages were all searched: no note about older mail.
  await expect(page.locator('p[data-mode="scan"]')).toHaveCount(0);

  // The conversation mirror of be-1 (hosted by the real node), with a live line.
  await page.goto(`${s.base}/#/org/be-1`);
  const log = page.getByTestId('transcript-log');
  if (s.node) {
    await expect(log).toContainText('I will add the trigram index', { timeout: 20_000 });
    await expect(log.locator('.k-notice').first()).toContainText('[amail]');
    await expect(log).toContainText('<b>Sending</b>');
    fs.appendFileSync(
      s.transcript,
      JSON.stringify({ type: 'assistant', sessionId: 'e2e-session', timestamp: new Date().toISOString(), message: { role: 'assistant', content: [{ type: 'text', text: 'LIVE LINE from the growing file' }] } }) + '\n',
    );
    await expect(log).toContainText('LIVE LINE from the growing file', { timeout: 15_000 });
  } else {
    await expect(log.locator('.explain')).toBeVisible();
  }
  // A seat no node hosts explains why there is nothing to show.
  await page.goto(`${s.base}/#/org/docs`);
  await expect(page.getByTestId('transcript-log').locator('.explain')).toBeVisible();

  // Needs you: the urgent question asks for an acknowledgement; `a` acks it.
  await page.goto(`${s.base}/#/needs`);
  const item = page.locator('.item[data-kind="ack"]', { hasText: 'Should short queries scan or fail?' });
  await expect(item).toBeVisible();
  await expect(page.locator('.item').first()).toHaveAttribute('data-kind', 'ack');
  await expect(page.locator('.item[data-kind="seat"]', { hasText: 'fe-lead' })).toBeVisible();
  // Its `attention` request is a card above the entries.
  await expect(page.getByTestId('needs-requests').locator('.rcard', { hasText: 'fe-lead' })).toBeVisible();
  await item.click();
  await expect(page.locator('.reader')).toContainText('Scan the most recent 2,000 messages');
  await page.keyboard.press('a');
  await expect(item).toHaveCount(0);
});

test('a large thread: older bodies are loaded on request', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  const root = await api<{ id: number }>(s.userToken, 'POST', '/messages', { to: ['owner'], subject: 'Large thread', body: 'root' });
  // Five replies of 250 KB: the server sends the bodies of the newest four.
  const big = (c: string) => c.repeat(250_000);
  const ids: number[] = [];
  for (const c of ['a', 'b', 'c', 'd', 'e']) {
    const r = await api<{ id: number }>(s.userToken, 'POST', '/messages', { to: ['owner'], subject: 'part', body: big(c), in_reply_to: root.id });
    ids.push(r.id);
  }
  await page.goto(`${s.base}/#/thread/${root.id}`);
  const oldest = page.locator(`article[data-msg="${ids[0]}"]`);
  await expect(oldest.getByTestId('body-omitted')).toBeVisible();
  await expect(page.locator(`article[data-msg="${ids[4]}"] pre.body`)).toContainText('eeee');
  await oldest.getByRole('button', { name: 'Show message' }).click();
  await expect(oldest.locator('pre.body')).toContainText('aaaa');
  await expect(oldest.getByTestId('body-omitted')).toHaveCount(0);
});

test('the user deletes an internal thread', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  const root = await api<{ id: number }>(s.userToken, 'POST', '/messages', { to: ['owner'], subject: 'Throwaway', body: 'one' });
  await api(s.userToken, 'POST', '/messages', { to: ['owner'], subject: 'Re: Throwaway', body: 'two', in_reply_to: root.id });
  const before = await api<{ stored_bytes: number }>(s.userToken, 'GET', '/org');
  await page.goto(`${s.base}/#/thread/${root.id}`);
  await expect(page.locator(`article[data-msg="${root.id}"]`)).toBeVisible();
  page.once('dialog', (d) => void d.accept());
  await page.getByRole('button', { name: 'Delete' }).click();
  await expect(page.getByText('Deleted 2 messages.')).toBeVisible();
  // The thread is gone; the page does not ask for it again.
  await expect(page.getByRole('link', { name: 'Go to the archive' })).toBeVisible();
  await expect(page.getByText('Thread not found.', { exact: false })).toHaveCount(0);
  const after = await api<{ stored_bytes: number }>(s.userToken, 'GET', '/org');
  expect(after.stored_bytes).toBeLessThan(before.stored_bytes);
});
