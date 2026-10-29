// Requests for the person in the web interface (M10): the cards of "needs
// you" (answer by option, free text, snooze, the defaults of a class, an
// answer that lost the race), the request page and the list, the option
// buttons above a request's mail, `blocked-on-human` on tasks, and the
// seat page's prompt box and interrupt. The requests are made here through
// the API with seat tokens; what stays open at the end is withdrawn and its
// mail marked read, so that the specs after this one see the queue as
// before.

import fs from 'node:fs';
import path from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { api, login, pushPresence, SCREENSHOTS, state } from './env';

test.describe.configure({ mode: 'serial' });

interface Req {
  num: number;
  state: string;
  thread?: number;
  rev: number;
  answer?: { option?: string; text?: string; by: string; note?: string };
}

const made: number[] = [];
const tasks: number[] = [];

async function ask(seat: string, body: Record<string, unknown>): Promise<Req> {
  const r = (await api<{ request: Req }>(state().seatTokens[seat], 'POST', '/requests', body)).request;
  made.push(r.num);
  return r;
}

async function getReq(num: number): Promise<Req> {
  return (await api<{ request: Req }>(state().userToken, 'GET', `/requests/${num}`)).request;
}

async function task(title: string, assignee: string): Promise<number> {
  const n = (await api<{ task: { num: number } }>(state().userToken, 'POST', '/tasks', { title, assignee })).task.num;
  tasks.push(n);
  return n;
}

async function shot(page: Page, name: string) {
  fs.mkdirSync(SCREENSHOTS, { recursive: true });
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(SCREENSHOTS, `${name}.png`) });
}

const card = (page: Page, num: number) => page.locator(`.rcard[data-request="${num}"]`);
const YESNO = [
  { key: 'yes', label: 'Ship it' },
  { key: 'no', label: 'Hold it', description: 'Wait for the benchmark' },
];

test.beforeAll(async () => {
  await pushPresence();
});

test.afterAll(async () => {
  const s = state();
  const ids: number[] = [];
  for (const num of made) {
    const r = await getReq(num);
    if (r.state === 'open') await api(s.userToken, 'POST', `/requests/${num}/withdraw`, {});
    if (r.thread) {
      const th = await api<{ messages: { id: number }[] }>(s.userToken, 'GET', `/threads/${r.thread}`);
      ids.push(...th.messages.map((m) => m.id));
    }
  }
  if (ids.length) await api(s.userToken, 'POST', '/inbox/read', { ids });
  for (const n of tasks) await api(s.userToken, 'PATCH', `/tasks/${n}`, { state: 'cancelled' }).catch(() => undefined);
});

test('needs you: request cards by class; an answer by option', async ({ page }) => {
  const s = state();
  const tn = await task('Trigram index for search', 'be-1');
  const r = await ask('be-1', {
    title: 'Ship the trigram index now?',
    body: 'The index is built.\n\n- **Ship**: merge today\n- Hold: wait\n\n<script>window.__pwned = 1</script>[bad](javascript:alert(1))',
    options: YESNO,
    default: 'yes',
    class: 'soon',
    task: tn,
  });
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/needs`);
  const reqs = page.getByTestId('needs-requests');
  await expect(reqs).toBeVisible();
  // The attention requests of the seats that need a person are cards in
  // the `now` group, before the others.
  await expect(reqs.locator('.rgroup').first()).toHaveAttribute('data-class', 'now');
  await expect(reqs.locator('.rgroup[data-class="now"] .rcard', { hasText: 'fe-lead' })).toBeVisible();
  // The seat's entry stays below, with the details of its condition.
  await expect(page.locator('.item[data-kind="seat"]', { hasText: 'fe-lead' })).toBeVisible();
  const c = card(page, r.num);
  await expect(c).toBeVisible();
  await expect(c.locator('xpath=ancestor::section[1]')).toHaveAttribute('data-class', 'soon');
  await expect(c).toContainText('be-1');
  await expect(c.getByTestId('req-task')).toHaveText(`t${tn}`);
  await expect(c.getByTestId('req-deadline')).toContainText('Default “Ship it” at');
  await expect(c.locator('button[data-option="yes"]')).toContainText('default');
  await expect(c.locator('button[data-option="no"]')).toHaveAttribute('title', 'Wait for the benchmark');
  // The body is collapsed, rendered and sanitized.
  const details = c.locator('details.body');
  await expect(details).not.toHaveAttribute('open', '');
  await details.locator('summary').click();
  await expect(details.locator('strong')).toHaveText('Ship');
  await expect(details.locator('script')).toHaveCount(0);
  await expect(details.locator('a[href^="javascript"]')).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();
  // The creation mail is not listed again under the requests.
  await expect(page.locator('.item', { hasText: `[r${r.num}]` })).toHaveCount(0);
  await shot(page, 'web-needs-requests');

  const nav = page.locator('nav.nav a', { hasText: 'Needs you' }).locator('.count');
  const before = Number(await nav.textContent());
  await c.getByRole('button', { name: 'Add a note' }).click();
  await c.getByLabel('Note').fill('after the benchmark');
  await c.locator('button[data-option="no"]').click();
  await expect(c.getByTestId('req-outcome')).toContainText('Answered by owner');
  await expect(c.getByTestId('req-outcome')).toContainText('Hold it');
  const done = await getReq(r.num);
  expect(done.state).toBe('answered');
  expect(done.answer).toMatchObject({ option: 'no', by: 'owner', note: 'after the benchmark' });
  await expect(nav).toHaveText(String(before - 1));
  await c.getByRole('button', { name: 'Hide' }).click();
  await expect(c).toHaveCount(0);
});

test('a free-text answer, and an answer that lost the race', async ({ page }) => {
  const s = state();
  const free = await ask('be-1', { title: 'Which tokenizer for Chinese?', free_text: true, class: 'soon' });
  const race = await ask('docs', { title: 'Publish the guide?', options: YESNO, class: 'soon' });
  await login(page, s.userToken, { lang: 'en' });
  // Without live updates the page keeps showing the request as it loaded it.
  await page.route('**/v1/events', (r) => r.abort());
  await page.goto(`${s.base}/#/needs`);
  const f = card(page, free.num);
  await f.getByLabel('Answer in your own words').fill('Bigrams for CJK, words for the rest');
  await f.getByRole('button', { name: 'Send answer' }).click();
  await expect(f.getByTestId('req-outcome')).toContainText('Bigrams for CJK, words for the rest');
  expect((await getReq(free.num)).answer).toMatchObject({ text: 'Bigrams for CJK, words for the rest', by: 'owner' });

  const c = card(page, race.num);
  await expect(c).toBeVisible();
  await api(s.userToken, 'POST', `/requests/${race.num}/answer`, { option: 'yes' });
  await c.locator('button[data-option="no"]').click();
  await expect(c.getByTestId('req-outcome')).toContainText('Someone answered first.');
  await expect(c.getByTestId('req-outcome')).toContainText('Ship it');
  expect((await getReq(race.num)).answer?.option).toBe('yes');
});

test('snooze hides a card for an hour in this browser', async ({ page }) => {
  const s = state();
  const r = await ask('fe-1', { title: 'Rename the search route?', options: YESNO, class: 'soon' });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/needs`);
  const c = card(page, r.num);
  await c.getByRole('button', { name: 'Snooze 1 h' }).click();
  await expect(c).toHaveCount(0);
  await expect(page.getByTestId('snoozed')).toContainText('1 snoozed until');
  await page.reload();
  await expect(page.getByTestId('needs-requests')).toBeVisible();
  await expect(card(page, r.num)).toHaveCount(0);
  // The request itself is unchanged.
  expect((await getReq(r.num)).state).toBe('open');
  await page.getByTestId('snoozed').getByRole('button', { name: 'Show them' }).click();
  await expect(card(page, r.num)).toBeVisible();
});

test('answer a class with its defaults', async ({ page }) => {
  const s = state();
  const a = await ask('be-1', { title: 'Keep the old index for a week?', options: YESNO, default: 'yes', class: 'later' });
  const b = await ask('fe-1', { title: 'Show match counts?', options: YESNO, default: 'no', class: 'later' });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/needs`);
  const group = page.locator('.rgroup[data-class="later"]');
  await expect(group.locator('.rcard')).toHaveCount(2);
  let asked = '';
  page.once('dialog', (d) => {
    asked = d.message();
    void d.accept();
  });
  await group.getByTestId('answer-defaults').click();
  await expect(group).toContainText('2 answered with the default.');
  expect(asked).toContain('2 “later” requests');
  expect((await getReq(a.num)).answer).toMatchObject({ option: 'yes', by: 'owner' });
  expect((await getReq(b.num)).answer).toMatchObject({ option: 'no', by: 'owner' });
});

test('the request page and the list', async ({ page }) => {
  const s = state();
  const tn = await task('Search page copy', 'fe-1');
  const r = await ask('fe-1', { title: 'Which empty-result text?', body: 'See the two drafts.', options: YESNO, class: 'soon', task: tn });
  const w = await ask('fe-1', { title: 'Drop the old route?', options: YESNO, class: 'soon' });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/requests/r${r.num}`);
  await expect(page.locator('h1')).toContainText('Which empty-result text?');
  await expect(page.getByTestId('req-state')).toHaveText('open');
  const facts = page.getByTestId('req-facts');
  await expect(facts).toContainText('fe-1');
  await expect(facts.getByRole('link', { name: `t${tn}` })).toBeVisible();
  await expect(facts.getByTestId('req-thread')).toBeVisible();
  await expect(page.getByTestId('req-events')).toContainText('created');
  await expect(page.locator('.mode-page [data-testid="doc-body"]')).toContainText('See the two drafts.');
  await page.locator('.mode-page button[data-option="yes"]').click();
  await expect(page.getByTestId('req-state')).toHaveText('answered');
  await expect(page.getByTestId('req-events')).toContainText('answered');
  await expect(page.getByTestId('req-outcome')).toContainText('Ship it');

  // Withdraw.
  await page.goto(`${s.base}/#/requests/r${w.num}`);
  page.once('dialog', (d) => void d.accept());
  await page.getByRole('button', { name: 'Withdraw' }).click();
  await expect(page.getByTestId('req-state')).toHaveText('withdrawn');

  // The list with filters.
  await page.goto(`${s.base}/#/requests?state=all&seat=fe-1`);
  const table = page.getByTestId('req-table');
  await expect(table.locator(`tr[data-request="${r.num}"]`)).toContainText('Ship it');
  await expect(table.locator(`tr[data-request="${w.num}"]`)).toContainText('withdrawn');
  await page.getByTestId('req-filters').getByLabel('State').selectOption('withdrawn');
  await expect(page).toHaveURL(/state=withdrawn/);
  await expect(table.locator(`tr[data-request="${w.num}"]`)).toBeVisible();
  await expect(table.locator(`tr[data-request="${r.num}"]`)).toHaveCount(0);
  await table.getByRole('link', { name: `r${w.num}` }).click();
  await expect(page).toHaveURL(new RegExp(`#/requests/r${w.num}$`));
});

test('the inbox: a request mail answers through its buttons; the answer mail shows the answer', async ({ page }) => {
  const s = state();
  const r = await ask('be-1', { title: 'Raise the scan limit to 5,000?', options: YESNO, class: 'soon' });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/inbox`);
  await page.locator('.mrow', { hasText: `[r${r.num}]` }).click();
  const strip = page.getByTestId('request-strip');
  await expect(strip.locator(`.rcard[data-request="${r.num}"]`)).toBeVisible();
  await strip.locator('button[data-option="no"]').click();
  await expect(strip.getByTestId('req-outcome')).toContainText('Hold it');
  const done = await getReq(r.num);
  expect(done.answer?.option).toBe('no');
  // The thread holds the answer mail; the page shows the answer once.
  await page.goto(`${s.base}/#/thread/${done.thread}`);
  await expect(page.locator('article[data-msg]')).toHaveCount(2);
  await expect(page.locator('article[data-msg]').nth(1)).toContainText(`Re: [r${r.num}]`);
  await expect(page.getByTestId('request-strip').getByTestId('req-outcome')).toContainText('Answered by owner');
  await expect(page.getByTestId('request-strip').locator('button[data-option]')).toHaveCount(0);
});

test('a task blocked on a request shows blocked-on-human', async ({ page }) => {
  const s = state();
  const tn = await task('Choose the ranking formula', 'be-2');
  const r = await ask('be-2', { title: 'BM25 or TF-IDF?', options: [{ key: 'bm25', label: 'BM25' }, { key: 'tfidf', label: 'TF-IDF' }], class: 'soon', task: tn });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/tasks/t${tn}`);
  const line = page.getByTestId('task-blocked-on-human');
  await expect(line).toContainText(`blocked-on-human (r${r.num})`);
  // Answering the request clears it without a reload.
  await api(s.userToken, 'POST', `/requests/${r.num}/answer`, { option: 'bm25' });
  await expect(line).toHaveCount(0);
  const r2 = await ask('be-2', { title: 'Boost titles?', options: YESNO, class: 'soon', task: tn });
  await expect(page.getByTestId('task-blocked-on-human')).toContainText(`(r${r2.num})`);
  await page.getByTestId('task-blocked-on-human').getByRole('link').click();
  await expect(page).toHaveURL(new RegExp(`#/requests/r${r2.num}$`));
  // The board's card says it too.
  await page.goto(`${s.base}/#/tasks`);
  await expect(page.locator(`.tcard[data-task="${tn}"]`).getByTestId('blocked-on-human')).toContainText(`r${r2.num}`);
});

test('the seat page: prompt, interrupt, the terminal for Claude Code, open requests', async ({ page }) => {
  const s = state();
  test.skip(!s.node, 'needs the e2e node');
  await pushPresence();
  const r = await ask('be-1', { title: 'Split the index migration?', options: YESNO, class: 'soon' });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/org/be-1`);
  await expect(page.getByTestId('seat-requests').locator(`li[data-request="${r.num}"]`)).toContainText('Split the index migration?');
  const controls = page.getByTestId('seat-controls');
  await controls.getByLabel('Prompt').fill('Please run the search tests again.');
  await controls.getByRole('button', { name: 'Send prompt' }).click();
  await expect(controls.getByTestId('control-msg')).toHaveText(/^Queued: sent to \d+ node\(s\)\.$/);
  await expect(controls.getByLabel('Prompt')).toHaveValue('');
  // Claude Code: no interrupt button; the terminal is the way.
  await expect(controls.getByTestId('interrupt')).toHaveCount(0);
  await expect(controls.getByTestId('no-interrupt')).toContainText('no structured interrupt');
  await shot(page, 'web-seat-controls');

  // A Codex seat has the button.
  await page.goto(`${s.base}/#/org/be-lead`);
  await page.getByTestId('interrupt').click();
  await expect(page.getByTestId('control-msg')).toHaveText(/^Interrupt sent to \d+ node\(s\)\.$/);
});

test('an interrupt the server refuses for a Claude Code seat is shown plainly', async ({ page }) => {
  const s = state();
  test.skip(!s.node, 'needs the e2e node');
  // The page believes be-1 runs Codex (its seat list says so, and no live
  // update corrects it); the server knows it runs Claude Code.
  await page.route('**/v1/events', (r) => r.abort());
  await page.route('**/v1/seats', async (route) => {
    const resp = await route.fetch();
    const seats = (await resp.json()) as { name: string; harness?: string; presence?: { harness?: string } }[];
    for (const x of seats) {
      if (x.name !== 'be-1') continue;
      x.harness = 'codex';
      if (x.presence) x.presence.harness = 'codex';
    }
    await route.fulfill({ response: resp, json: seats });
  });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/org/be-1`);
  await page.getByTestId('interrupt').click();
  await expect(page.getByTestId('control-msg')).toHaveText('This seat runs Claude Code, which has no structured interrupt. Stop the turn in its terminal.');
});

test('phone width: cards, buttons and the seat controls fit', async ({ page }) => {
  const s = state();
  await pushPresence();
  const r = await ask('be-1', {
    title: 'A long question whose title wraps on a narrow screen without making the page wider than the phone',
    body: 'A body with a long line: ' + 'x'.repeat(300),
    options: [
      { key: 'a', label: 'The first option, with a label long enough to wrap onto a second line' },
      { key: 'b', label: 'Second' },
      { key: 'c', label: 'Third' },
    ],
    free_text: true,
    default: 'b',
    class: 'soon',
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, s.userToken, { lang: 'zh' });
  const overflow = async () => {
    const doc = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    const main = await page.locator('#main').evaluate((m) => m.scrollWidth - m.clientWidth);
    return Math.max(doc, main);
  };
  await page.goto(`${s.base}/#/needs`);
  const c = card(page, r.num);
  await c.locator('details.body summary').click();
  for (const key of ['a', 'b', 'c']) {
    const b = c.locator(`button[data-option="${key}"]`);
    await b.scrollIntoViewIfNeeded();
    await expect(b).toBeInViewport();
    const box = (await b.boundingBox())!;
    expect(box.height).toBeGreaterThanOrEqual(28);
    expect(box.x + box.width).toBeLessThanOrEqual(390);
  }
  expect(await overflow()).toBeLessThanOrEqual(0);
  await shot(page, 'web-needs-requests-phone');
  // The buttons work at this width.
  await c.locator('button[data-option="c"]').click();
  await expect(c.getByTestId('req-outcome')).toBeVisible();

  await page.goto(`${s.base}/#/requests/r${r.num}`);
  await expect(page.getByTestId('req-facts')).toBeVisible();
  expect(await overflow()).toBeLessThanOrEqual(0);
  await page.goto(`${s.base}/#/requests?state=all`);
  await expect(page.getByTestId('req-table')).toBeVisible();
  expect(await overflow()).toBeLessThanOrEqual(0);

  await page.goto(`${s.base}/#/org/be-lead`);
  const send = page.getByTestId('seat-controls').getByRole('button', { name: '发送提示' });
  await send.scrollIntoViewIfNeeded();
  await expect(send).toBeInViewport();
  await expect(page.getByTestId('interrupt')).toBeInViewport();
  expect(await overflow()).toBeLessThanOrEqual(0);
});
