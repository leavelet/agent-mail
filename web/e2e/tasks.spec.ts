// Tasks in the web interface: the board with its columns and markers, live
// updates from changes made through the API, the priority control, ranking
// by dragging within a column, the task page's actions, and the task lists
// of the seat view, the thread view and "needs you". The tasks are created
// here through the API.

import fs from 'node:fs';
import path from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { api, login, SCREENSHOTS, state } from './env';

test.describe.configure({ mode: 'serial' });

interface Task {
  num: number;
  title: string;
  state: string;
  priority: string;
  rank: number;
  thread?: number;
}
interface Detail {
  task: Task;
}

const nums: Record<string, number> = {};

async function create(token: string, body: Record<string, unknown>): Promise<number> {
  return (await api<Detail>(token, 'POST', '/tasks', body)).task.num;
}

async function getTask(num: number): Promise<Task> {
  return (await api<Detail>(state().userToken, 'GET', `/tasks/${num}`)).task;
}

async function shot(page: Page, name: string) {
  fs.mkdirSync(SCREENSHOTS, { recursive: true });
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(SCREENSHOTS, `${name}.png`) });
}

const card = (page: Page, num: number) => page.locator(`.tcard[data-task="${num}"]`);
const column = (page: Page, s: string) => page.locator(`[data-column="${s}"]`);

test.beforeAll(async () => {
  const s = state();
  const u = s.userToken;
  nums.index = await create(u, { title: 'Index schema for search', assignee: 'be-1', priority: 'high', labels: ['search'] });
  nums.api = await create(u, { title: 'Search API endpoint', assignee: 'be-2' });
  await api(u, 'POST', `/tasks/${nums.api}/deps`, { blocked_by: nums.index });
  nums.page = await create(u, { title: 'Search page layout', assignee: 'fe-1' });
  await api(s.seatTokens['fe-1'], 'PATCH', `/tasks/${nums.page}`, { state: 'blocked', waiting_on: 'owner', blocked_reason: 'Pick one of the two layouts' });
  nums.d = await create(u, { title: 'Highlight matches', assignee: 'fe-2' });
  nums.e = await create(u, { title: 'Paging of results', assignee: 'fe-2' });
  nums.f = await create(u, { title: 'Empty result message', assignee: 'fe-2' });
  nums.late = await create(u, { title: 'Write the release note', assignee: 'owner', due: new Date(Date.now() - 3_600_000).toISOString() });
});

test('the board shows one column per state with cards and markers', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/tasks`);
  await expect(page.locator('nav a.active')).toHaveText(/^Tasks$/);
  const board = page.getByTestId('task-board');
  await expect(board.locator('[data-column]')).toHaveCount(7);
  for (const st of ['ready', 'open', 'doing', 'blocked', 'review', 'done', 'cancelled']) await expect(column(page, st)).toBeVisible();
  await expect(column(page, 'ready').locator(`.tcard[data-task="${nums.index}"]`)).toContainText('Index schema for search');
  await expect(card(page, nums.index)).toContainText('be-1');
  await expect(card(page, nums.index).locator('select.prio')).toHaveValue('high');
  // An open task with a blocker, and a blocked one.
  await expect(column(page, 'open').locator(`.tcard[data-task="${nums.api}"]`).getByTestId('waits-on')).toHaveText(`waits on t${nums.index}`);
  await expect(column(page, 'blocked').locator(`.tcard[data-task="${nums.page}"]`).getByTestId('waits-for')).toContainText('waits for owner');
  await expect(column(page, 'blocked').getByTestId('column-count')).toHaveText('1');
  await expect(column(page, 'ready').getByTestId('column-count')).toHaveText('5');
  // Urgent and high first, then by rank.
  const order = await column(page, 'ready').locator('.tcard').evaluateAll((els) => els.map((e) => Number((e as HTMLElement).dataset.task)));
  expect(order).toEqual([nums.index, nums.d, nums.e, nums.f, nums.late]);
  await shot(page, 'web-tasks-board');
});

test('dragging a card within its column sets its rank', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/tasks`);
  const ready = column(page, 'ready');
  await expect(ready.locator(`.tcard[data-task="${nums.f}"]`)).toBeVisible();
  const before = await getTask(nums.d);
  const sent = page.waitForRequest((r) => r.method() === 'PATCH' && r.url().endsWith(`/v1/tasks/${nums.f}`));
  // Drop t(f) on the upper half of t(d): it lands before it.
  await ready.locator(`[data-slot="${nums.f}"]`).dragTo(ready.locator(`[data-slot="${nums.d}"]`), { targetPosition: { x: 20, y: 4 } });
  const req = await sent;
  const body = req.postDataJSON() as Record<string, unknown>;
  expect(Object.keys(body)).toEqual(['rank']);
  expect(body.rank).toBeLessThan(before.rank);
  await expect.poll(async () => (await getTask(nums.f)).rank).toBe(body.rank);
  const order = await ready.locator('.tcard').evaluateAll((els) => els.map((e) => Number((e as HTMLElement).dataset.task)));
  expect(order.slice(0, 3)).toEqual([nums.index, nums.f, nums.d]);
  // Another column is no drop target: nothing is sent.
  let patched = false;
  page.on('request', (r) => {
    if (r.method() === 'PATCH') patched = true;
  });
  await ready.locator(`[data-slot="${nums.e}"]`).dragTo(column(page, 'doing').locator('.cards'));
  await page.waitForTimeout(500);
  expect(patched).toBe(false);
  expect((await getTask(nums.e)).state).toBe('ready');
});

test('the priority control on a card sends the priority', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/tasks`);
  const sel = card(page, nums.e).locator('select.prio');
  await expect(sel).toHaveValue('normal');
  const sent = page.waitForRequest((r) => r.method() === 'PATCH' && r.url().endsWith(`/v1/tasks/${nums.e}`));
  await sel.selectOption('urgent');
  expect((await sent).postDataJSON()).toEqual({ priority: 'urgent' });
  await expect.poll(async () => (await getTask(nums.e)).priority).toBe('urgent');
  // Urgent goes to the top of its column.
  await expect(column(page, 'ready').locator('.tcard').first()).toHaveAttribute('data-task', String(nums.e));
});

test('a change made through the API shows up without reloading', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/tasks`);
  await expect(column(page, 'ready').locator(`.tcard[data-task="${nums.d}"]`)).toBeVisible();
  // fe-2 starts the task and lowers its priority, as an agent would.
  await api(s.seatTokens['fe-2'], 'PATCH', `/tasks/${nums.d}`, { state: 'doing', priority: 'low' });
  await expect(column(page, 'doing').locator(`.tcard[data-task="${nums.d}"]`)).toBeVisible();
  await expect(card(page, nums.d).locator('select.prio')).toHaveValue('low');
  await expect(column(page, 'ready').locator(`.tcard[data-task="${nums.d}"]`)).toHaveCount(0);
  // A new task appears too.
  const n = await create(s.seatTokens['be-lead'], { title: 'Live created task' });
  await expect(column(page, 'ready').locator(`.tcard[data-task="${n}"]`)).toContainText('Live created task');
});

test('the table filters tasks', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/tasks`);
  await page.getByRole('tab', { name: 'Table' }).click();
  const table = page.getByTestId('task-table');
  await expect(table.locator(`tr[data-task="${nums.index}"]`)).toBeVisible();
  await page.getByTestId('task-filters').getByLabel('Assignee').selectOption('fe-2');
  await expect(table.locator(`tr[data-task="${nums.index}"]`)).toHaveCount(0);
  await expect(table.locator(`tr[data-task="${nums.e}"]`)).toBeVisible();
  await page.getByTestId('task-filters').getByLabel('Assignee').selectOption('');
  await page.locator('input[type=search]').fill('Paging');
  await expect(table.locator('tbody tr[data-task]')).toHaveCount(1);
  await page.locator('input[type=search]').fill('');
  // Back to the board for the next tests (the view is remembered).
  await page.getByRole('tab', { name: 'Board' }).click();
});

test('the task page changes the state, comments and shows errors', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/tasks/t${nums.f}`);
  await expect(page.locator('h1')).toContainText('Empty result message');
  await expect(page.getByTestId('task-state')).toHaveText('ready');
  const form = page.getByTestId('task-state-form');
  await form.getByLabel('Set state').selectOption('doing');
  await form.getByRole('button', { name: 'Set state' }).click();
  await expect(page.getByTestId('task-state')).toHaveText('doing');
  expect((await getTask(nums.f)).state).toBe('doing');
  await expect(page.getByTestId('task-history')).toContainText('state: open → doing');

  // A comment starts the discussion thread.
  await page.getByLabel('Comment', { exact: true }).fill('Please show the query in the message.');
  await page.getByRole('button', { name: 'Comment', exact: true }).click();
  await expect(page.locator('p.ok[role=status]')).toContainText('Comment sent');
  await expect(page.locator('.discussion article')).toContainText('Please show the query in the message.');
  const thread = (await getTask(nums.f)).thread;
  expect(thread).toBeTruthy();
  await expect(page.getByTestId('task-history')).toContainText('commented');
  await shot(page, 'web-task-page');

  // The thread view lists the task under the subject.
  await page.goto(`${s.base}/#/thread/${thread}`);
  await expect(page.getByTestId('thread-tasks')).toContainText(`t${nums.f}`);
  await expect(page.getByTestId('thread-tasks')).toContainText('Empty result message');

  // A dependency that makes a cycle is refused with the server's code in words.
  await page.goto(`${s.base}/#/tasks/t${nums.index}`);
  await expect(page.locator('section.refs').first()).toBeVisible();
  await page.getByLabel('Add dependency').first().fill(`t${nums.api}`);
  await page.getByRole('button', { name: 'Add dependency' }).click();
  await expect(page.getByTestId('task-error')).toContainText('This dependency would make a cycle.');
  // The blocked list links to the waiting task.
  await expect(page.locator(`li[data-ref="${nums.api}"] a`).first()).toHaveAttribute('href', `#/tasks/t${nums.api}`);
});

test('the task page follows changes made elsewhere', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/tasks/t${nums.api}`);
  await expect(page.getByTestId('task-state')).toHaveText('open');
  // The last blocker is done: the task becomes ready.
  await api(s.seatTokens['be-1'], 'PATCH', `/tasks/${nums.index}`, { state: 'done' });
  await expect(page.getByTestId('task-state')).toHaveText('ready');
  await expect(page.locator(`li[data-ref="${nums.index}"]`)).toContainText('done');
});

test('a request to take over shows on the card and the page hands the task over', async ({ page }) => {
  const s = state();
  const n = await create(s.userToken, { title: 'Result caching', assignee: 'fe-1' });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/tasks`);
  await expect(card(page, n)).toBeVisible();
  await expect(card(page, n).getByTestId('steal')).toHaveCount(0);
  // fe-2 asks through the API: the card shows it without a reload.
  await api(s.seatTokens['fe-2'], 'POST', `/tasks/${n}/steal`);
  await expect(card(page, n).getByTestId('steal')).toContainText('fe-2 asks to take over');
  await page.goto(`${s.base}/#/tasks/t${n}`);
  const box = page.getByTestId('task-steal');
  await expect(box).toContainText('fe-2 asks to take over');
  await expect(box.getByRole('button', { name: 'Keep with fe-1' })).toBeVisible();
  await box.getByRole('button', { name: 'Hand over to fe-2' }).click();
  await expect(page.getByTestId('task-steal')).toHaveCount(0);
  await expect(page.getByLabel('Assignee')).toHaveValue('fe-2');
  const after = await api<{ task: { assignee?: string; steal_requested_by?: string } }>(s.userToken, 'GET', `/tasks/${n}`);
  expect(after.task.assignee).toBe('fe-2');
  expect(after.task.steal_requested_by).toBeUndefined();
});

test('seat view, needs you: the task lists', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/org/fe-2`);
  const lists = page.getByTestId('seat-tasks');
  await expect(lists.locator('[data-list="mine"]')).toContainText(`t${nums.e}`);
  await expect(lists.locator('[data-list="mine"]')).toContainText(`t${nums.d}`);
  // be-2's task waits on nothing now; fe-1's task waits for the user.
  await page.goto(`${s.base}/#/needs`);
  const needs = page.getByTestId('needs-tasks');
  await expect(needs).toContainText(`t${nums.page}`);
  await expect(needs).toContainText('Search page layout');
  await expect(needs).toContainText(`t${nums.late}`);
  // The user's own seat view has the same in "Others wait for".
  await page.goto(`${s.base}/#/org/owner`);
  await expect(page.getByTestId('seat-tasks').locator('[data-list="owed"]')).toContainText(`t${nums.page}`);
});

test('the board works at phone width without horizontal page scroll', async ({ page }) => {
  const s = state();
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, s.userToken, { lang: 'zh', theme: 'dark' });
  await page.goto(`${s.base}/#/tasks`);
  await expect(page.getByTestId('task-board')).toBeVisible();
  await expect(column(page, 'ready').locator('header')).toContainText('可开始');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  const main = await page.locator('#main').evaluate((el) => el.scrollWidth - el.clientWidth);
  expect(main).toBeLessThanOrEqual(0);
  await shot(page, 'web-tasks-board-phone');
});
