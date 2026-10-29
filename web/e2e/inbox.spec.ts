// The Inbox's check boxes, the selection bar (Mark read, Mark unread,
// Clear selection), the `x` key and "Mark all read", at desktop and phone
// width. These tests use the user of the 1,000-seat org (`scale`), whose
// Inbox no other test reads, so that marking all of it read changes
// nothing the other tests expect.

import fs from 'node:fs';
import path from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { api, login, SCREENSHOTS, state } from './env';

async function shot(page: Page, name: string) {
  fs.mkdirSync(SCREENSHOTS, { recursive: true });
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(SCREENSHOTS, `${name}.png`) });
}

test.describe.configure({ mode: 'serial' });

const SENDERS = ['team00-lead', 'team01-lead', 'team02-lead', 'team03-lead', 'team04-lead'];
const tokens: Record<string, string> = {};
let sent = 0;

function big() {
  const s = state();
  return { base: s.base, user: s.big!.userToken };
}

/** Mail to the user of `scale`, from its agent seats in turn (each seat may
 * send 30 messages a minute). */
async function mail(subjects: string[]): Promise<number[]> {
  const { user } = big();
  const ids: number[] = [];
  for (const subject of subjects) {
    const from = SENDERS[sent++ % SENDERS.length];
    tokens[from] ??= (await api<{ token: string }>(user, 'POST', `/seats/${from}/tokens`, { label: 'e2e inbox' })).token;
    ids.push((await api<{ id: number }>(tokens[from], 'POST', '/messages', { to: ['owner'], subject, body: `About ${subject}.` })).id);
  }
  return ids;
}

async function unreadCount(): Promise<number> {
  const c = await api<{ seats: { seat: string; unread: number }[] }>(big().user, 'GET', '/counts?seat=owner');
  return c.seats[0].unread;
}

async function readAllByApi() {
  await api(big().user, 'POST', '/inbox/read', { all: true });
}

const row = (page: Page, subject: string) => page.locator('.mrow', { hasText: subject });
const box = (page: Page, subject: string) => row(page, subject).getByTestId('row-check');
const navCount = (page: Page) => page.locator('nav.nav a', { hasText: 'Inbox' }).locator('.count');

test.beforeAll(async () => {
  await readAllByApi();
});

test('check boxes select rows; the selection bar marks them read and unread', async ({ page }) => {
  const tag = `sel-${Date.now()}`;
  const subjects = [`${tag} one`, `${tag} two`, `${tag} three`];
  await mail(subjects);
  await login(page, big().user, { lang: 'en' });
  await page.goto(`${big().base}/#/inbox`);
  for (const s of subjects) await expect(row(page, s)).toHaveClass(/unread/);
  await expect(navCount(page)).toHaveText('3');
  const bar = page.getByTestId('selection-bar');
  await expect(bar).toHaveCount(0);

  // Ticking a check box selects the row for the bar, without opening it.
  await box(page, subjects[0]).check();
  await box(page, subjects[1]).check();
  await expect(bar).toBeVisible();
  await expect(page.getByTestId('selected-count')).toHaveText('2 selected');
  await expect(row(page, subjects[0])).toHaveClass(/unread/);
  await shot(page, 'web-inbox-selection');

  await bar.getByRole('button', { name: 'Mark read' }).click();
  await expect(row(page, subjects[0])).not.toHaveClass(/unread/);
  await expect(row(page, subjects[1])).not.toHaveClass(/unread/);
  await expect(row(page, subjects[2])).toHaveClass(/unread/);
  await expect(navCount(page)).toHaveText('1');
  expect(await unreadCount()).toBe(1);
  // The selection stays for the next action.
  await expect(page.getByTestId('selected-count')).toHaveText('2 selected');

  await bar.getByRole('button', { name: 'Mark unread' }).click();
  await expect(row(page, subjects[0])).toHaveClass(/unread/);
  await expect(row(page, subjects[1])).toHaveClass(/unread/);
  await expect(navCount(page)).toHaveText('3');
  expect(await unreadCount()).toBe(3);

  await bar.getByRole('button', { name: 'Clear selection' }).click();
  await expect(bar).toHaveCount(0);
  await expect(box(page, subjects[0])).not.toBeChecked();

  // The header check box selects every loaded row, and clears them again.
  const all = page.getByTestId('check-all');
  await all.check();
  const n = await page.locator('.mrow').count();
  await expect(page.getByTestId('selected-count')).toHaveText(`${n} selected`);
  for (const s of subjects) await expect(box(page, s)).toBeChecked();
  await all.uncheck();
  await expect(bar).toHaveCount(0);

  // `x` ticks the row the keyboard is on.
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.keyboard.press('Home');
  const first = page.locator('.mrow.sel');
  await page.keyboard.press('x');
  await expect(first.getByTestId('row-check')).toBeChecked();
  await expect(page.getByTestId('selected-count')).toHaveText('1 selected');
  await page.keyboard.press('x');
  await expect(first.getByTestId('row-check')).not.toBeChecked();

  // The selection belongs to the tab: changing tabs clears it.
  await box(page, subjects[2]).check();
  await expect(bar).toBeVisible();
  await page.getByRole('tab', { name: 'Sent' }).click();
  await expect(page.getByTestId('row-check')).toHaveCount(0);
  await page.getByRole('tab', { name: /Inbox/ }).click();
  await expect(box(page, subjects[2])).not.toBeChecked();
  await expect(bar).toHaveCount(0);

  await readAllByApi();
});

test('Mark all read marks the list read, asking first above 20', async ({ page }) => {
  const tag = `all-${Date.now()}`;
  await mail([`${tag} a`, `${tag} b`]);
  await login(page, big().user, { lang: 'en' });
  await page.goto(`${big().base}/#/inbox`);
  const button = page.getByTestId('mark-all-read');
  await expect(row(page, `${tag} a`)).toHaveClass(/unread/);

  // Two: no question.
  let asked = 0;
  page.on('dialog', (d) => {
    asked++;
    void d.accept();
  });
  await button.click();
  await expect(row(page, `${tag} a`)).not.toHaveClass(/unread/);
  await expect(row(page, `${tag} b`)).not.toHaveClass(/unread/);
  await expect(navCount(page)).toHaveCount(0);
  await expect(button).toBeDisabled();
  expect(asked).toBe(0);
  expect(await unreadCount()).toBe(0);

  // 22: asked once, with the count.
  const many = Array.from({ length: 22 }, (_, i) => `${tag} m${i}`);
  page.removeAllListeners('dialog');
  let question = '';
  page.on('dialog', (d) => {
    asked++;
    question = d.message();
    void d.accept();
  });
  await mail(many);
  await expect(navCount(page)).toHaveText('22');
  // The list has the new mail ("Mark all read" leaves mail newer than the
  // list unread).
  await expect(row(page, `${tag} m21`)).toBeVisible();
  await expect(button).toBeEnabled();
  await button.click();
  await expect(navCount(page)).toHaveCount(0);
  expect(asked).toBe(1);
  expect(question).toContain('22');
  await expect(page.locator('.mrow.unread')).toHaveCount(0);
  expect(await unreadCount()).toBe(0);

  // Declining changes nothing.
  await mail(Array.from({ length: 21 }, (_, i) => `${tag} n${i}`));
  await expect(navCount(page)).toHaveText('21');
  await expect(row(page, `${tag} n20`)).toBeVisible();
  page.removeAllListeners('dialog');
  page.on('dialog', (d) => void d.dismiss());
  await button.click();
  await expect(page.locator('.mrow.unread')).toHaveCount(21);
  expect(await unreadCount()).toBe(21);
  await readAllByApi();
});

test('at phone width the check boxes work and the selection bar leaves the list visible', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const tag = `phone-${Date.now()}`;
  const subjects = [`${tag} one`, `${tag} two`, `${tag} three`];
  await mail(subjects);
  await login(page, big().user, { lang: 'zh' });
  await page.goto(`${big().base}/#/inbox`);
  await expect(row(page, subjects[0])).toHaveClass(/unread/);

  // Large enough to tap.
  const b = await box(page, subjects[0]).boundingBox();
  expect(b!.width).toBeGreaterThanOrEqual(20);
  expect(b!.height).toBeGreaterThanOrEqual(20);
  await box(page, subjects[0]).click();
  await box(page, subjects[1]).click();
  const bar = page.getByTestId('selection-bar');
  await expect(bar).toBeVisible();
  await expect(page.getByTestId('selected-count')).toHaveText('已选 2 封');

  // The bar sits above the list, not over it.
  const barBox = (await bar.boundingBox())!;
  const listBox = (await page.locator('.listpane .scroll').boundingBox())!;
  expect(barBox.y + barBox.height).toBeLessThanOrEqual(listBox.y + 1);
  // Nothing lies over the rows' check boxes.
  for (const s of subjects) {
    const check = box(page, s);
    await check.scrollIntoViewIfNeeded();
    const hit = await check.evaluate((el) => {
      const r = el.getBoundingClientRect();
      return document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2) === el;
    });
    expect(hit, s).toBe(true);
  }
  await shot(page, 'web-inbox-selection-phone');
  // No horizontal page scroll.
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);

  await bar.getByRole('button', { name: '标为已读' }).click();
  await expect(row(page, subjects[0])).not.toHaveClass(/unread/);
  await expect(row(page, subjects[1])).not.toHaveClass(/unread/);
  await expect(row(page, subjects[2])).toHaveClass(/unread/);
  expect(await unreadCount()).toBe(1);

  await page.getByTestId('mark-all-read').click();
  await expect(row(page, subjects[2])).not.toHaveClass(/unread/);
  expect(await unreadCount()).toBe(0);
});
