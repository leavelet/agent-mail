// Documents and seat handover in the web interface: the create form with
// similar titles, the viewer (Markdown with section anchors, links, no
// script), a write refused for a stale revision and written again, the
// search, verification, live updates, "needs you", and the seat's
// Handover and Worklog tabs. The documents are created here through the
// API, as agents would.

import fs from 'node:fs';
import path from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { api, login, SCREENSHOTS, state } from './env';

test.describe.configure({ mode: 'serial' });

interface DocDetail {
  doc: { num: number; rev: number; status: string; title: string; verified_by?: string; stale?: boolean };
}

const nums: Record<string, number> = {};

async function newDoc(token: string, body: Record<string, unknown>): Promise<number> {
  return (await api<DocDetail>(token, 'POST', '/docs', { ...body, client_key: undefined })).doc.num;
}

async function getDoc(num: number): Promise<DocDetail['doc']> {
  return (await api<DocDetail>(state().userToken, 'GET', `/docs/${num}`)).doc;
}

async function shot(page: Page, name: string) {
  fs.mkdirSync(SCREENSHOTS, { recursive: true });
  await page.waitForTimeout(300);
  await page.screenshot({ path: path.join(SCREENSHOTS, `${name}.png`) });
}

const RUNBOOK = [
  '# Deploy',
  '',
  'How the search service is deployed. Read [the decision](d{decision}) first; the work is t{task}.',
  '',
  '## When',
  '',
  'After a merge into main.',
  '',
  '## Steps',
  '',
  '1. Build the image.',
  '2. Roll it out; see [rollback](#rollback).',
  '',
  '## Verify',
  '',
  'The health check answers 200. Old notes: d999999 (gone).',
  '',
  '## Rollback {#rollback}',
  '',
  'Redeploy the previous image tag.',
  '',
  '<script>window.__pwned = 1</script>',
  '<p onclick="window.__pwned = 2">click</p>',
  '[bad](javascript:window.__pwned=3)',
  '',
  ...Array.from({ length: 60 }, (_, i) => `Filler line ${i} so that the rollback section is below the fold.\n`),
].join('\n');

test.beforeAll(async () => {
  const s = state();
  const lead = s.seatTokens['be-lead'];
  // A task for the body to link to; cancelled, so that the task board
  // that tasks.spec.ts checks keeps its counts.
  nums.task = (await api<{ task: { num: number } }>(s.userToken, 'POST', '/tasks', { title: 'Deploy the search service' })).task.num;
  await api(s.userToken, 'PATCH', `/tasks/${nums.task}`, { state: 'cancelled' });
  nums.decision = await newDoc(lead, {
    title: 'Use trigram search for archives',
    kind: 'decision',
    description: 'Why the archive search uses trigrams',
    body: '## Context\nShort queries.\n## Decision\nTrigrams.\n## Alternatives\nWords.\n## Consequences\nCJK works.\n',
    team: 'backend',
  });
  nums.runbook = await newDoc(lead, {
    title: 'Deploy rollback procedure',
    kind: 'runbook',
    description: 'How to deploy and roll back the search service',
    body: RUNBOOK.replace('{decision}', String(nums.decision)).replace('{task}', String(nums.task)).replace(/\n\n(?=Filler)/g, '\n'),
    labels: ['deploy'],
    team: 'backend',
  });
  await api(lead, 'PATCH', `/docs/${nums.runbook}`, { pinned: true });
  await api(lead, 'PATCH', `/docs/${nums.runbook}`, { status: 'active' });
});

test('the landing page shows a team with pinned documents and counts', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en', theme: 'light' });
  await page.goto(`${s.base}/#/docs?team=backend`);
  await expect(page.locator('nav a.active')).toHaveText(/^Documents$/);
  const landing = page.getByTestId('docs-landing');
  await expect(landing.locator(`[data-doc="${nums.runbook}"]`).first()).toBeVisible();
  await expect(landing.locator('[data-kind="runbook"] td.num')).toHaveText('1');
  await expect(landing.locator('[data-kind="decision"] td.num')).toHaveText('1');
  await expect(page.getByTestId('docs-drafts')).toHaveText('1 draft(s)');
  // A kind opens its list with the filters.
  await landing.locator('[data-kind="decision"] a').click();
  await expect(page.getByTestId('docs-list').locator(`tr[data-doc="${nums.decision}"]`)).toBeVisible();
  await expect(page.getByTestId('docs-list').locator(`tr[data-doc="${nums.runbook}"]`)).toHaveCount(0);
  await shot(page, 'web-docs-landing');
});

test('the create form fills the required sections and shows similar titles', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/docs/new`);
  await page.getByTestId('doc-new-kind').selectOption('runbook');
  await expect(page.getByTestId('doc-new-body')).toHaveValue('## When\n\n## Steps\n\n## Verify\n\n');
  await page.locator('#doc-title').fill('Rollback procedure for search');
  const cands = page.getByTestId('doc-candidates');
  await expect(cands).toContainText(`d${nums.runbook}`);
  await expect(cands.locator(`a[href="#/docs/d${nums.runbook}"]`).first()).toBeVisible();
  await page.getByTestId('doc-new-body').fill('## When\nNever.\n## Steps\nNone.\n## Verify\nNothing.\n');
  await page.getByRole('button', { name: 'Create document' }).click();
  await expect(page).toHaveURL(/#\/docs\/d\d+$/);
  const num = Number(/d(\d+)$/.exec(page.url())![1]);
  nums.created = num;
  await expect(page.getByTestId('doc-status')).toHaveText('draft');
  await expect(page.getByTestId('doc-kind')).toHaveText('runbook');
  await expect(page.getByTestId('doc-body').locator('h2#when')).toHaveText(/When/);
  // The create answer's own candidates are shown once.
  await expect(page.getByTestId('doc-candidates')).toContainText('similar documents exist');
  expect((await getDoc(num)).title).toBe('Rollback procedure for search');
});

test('the viewer renders the body with anchors and links and no script', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/docs/d${nums.runbook}`);
  const body = page.getByTestId('doc-body');
  await expect(body.locator('h2#rollback')).toContainText('Rollback');
  await expect(body.locator('h2#steps')).toBeVisible();
  await expect(body.locator(`a[href="#/docs/d${nums.decision}"]`)).toHaveText('the decision');
  await expect(body.locator(`a[href="#/tasks/t${nums.task}"]`)).toHaveText(`t${nums.task}`);
  await expect(body.locator('script')).toHaveCount(0);
  await expect(body.locator('[onclick]')).toHaveCount(0);
  await expect(body.locator('a[href^="javascript"]')).toHaveCount(0);
  await body.getByText('click', { exact: true }).click();
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();
  // Fields, relations, links.
  await expect(page.getByTestId('doc-status')).toHaveText('active');
  await expect(page.getByTestId('doc-rev')).toHaveText('1');
  const links = page.getByTestId('doc-links');
  await expect(links.locator('tr', { hasText: 'd999999' }).locator('.chip')).toHaveText('dangling');
  await expect(links.locator('tr', { hasText: `d${nums.decision}` })).toContainText('Use trigram search for archives');
  // A section link inside the body scrolls to the section.
  await body.locator(`a[href="#/docs/d${nums.runbook}#rollback"]`).first().click();
  await expect(page).toHaveURL(new RegExp(`#/docs/d${nums.runbook}#rollback$`));
  await expect(body.locator('h2#rollback')).toBeInViewport();
  // Opened by a URL with the anchor.
  await page.goto(`${s.base}/#/docs/d${nums.decision}`);
  await expect(page.getByTestId('doc-body').locator('h2#decision')).toBeVisible();
  await page.goto(`${s.base}/#/docs/d${nums.runbook}#verify`);
  await expect(page.getByTestId('doc-body').locator('h2#verify')).toBeInViewport();
  await shot(page, 'web-doc-viewer');
});

test('a write against a stale revision shows the diff and can be sent again', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/docs/d${nums.created}`);
  await page.getByTestId('doc-edit').click();
  const input = page.getByTestId('doc-body-input');
  await expect(input).toHaveValue(/## When/);
  // be-lead writes a revision meanwhile.
  await api(s.userToken, 'PATCH', `/docs/${nums.created}`, { editors: ['be-lead'] });
  await api(s.seatTokens['be-lead'], 'PUT', `/docs/${nums.created}/body`, { body: '## When\nOn every release.\n## Steps\nNone.\n## Verify\nNothing.\n', message: 'be-lead: when' });
  await input.fill('## When\nNever.\n## Steps\nTwo steps.\n## Verify\nNothing.\n');
  await page.getByRole('button', { name: /Save as a new revision/ }).click();
  const conflict = page.getByTestId('doc-conflict');
  await expect(conflict).toContainText('now revision 2');
  await expect(conflict.getByTestId('doc-diff')).toContainText('+On every release.');
  await expect(conflict).toContainText('be-lead');
  // The text is still in the editor; send it again on top of revision 2.
  await expect(input).toHaveValue(/Two steps/);
  await conflict.getByTestId('doc-retry').click();
  await expect(conflict).toHaveCount(0);
  await expect(page.getByTestId('doc-rev')).toHaveText('3');
  await expect(page.getByTestId('doc-body')).toContainText('Two steps.');
  // The revisions and a diff between two of them.
  const hist = page.getByTestId('doc-history');
  await expect(hist.locator('tr[data-rev]')).toHaveCount(3);
  await hist.getByTestId('doc-diff-show').click();
  await expect(hist.getByTestId('doc-diff')).toContainText('-On every release.');
});

test('search finds sections with their heading path', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/docs`);
  await page.getByTestId('docs-search').fill('previous image');
  await page.getByTestId('docs-search').press('Enter');
  await expect(page).toHaveURL(/#\/docs\?q=previous/);
  const hit = page.getByTestId('docs-hits').locator(`[data-doc="${nums.runbook}"]`).first();
  await expect(hit).toContainText(`d${nums.runbook}#rollback`);
  await expect(hit).toContainText('Deploy > Rollback');
  await hit.locator(`a[href="#/docs/d${nums.runbook}#rollback"]`).first().click();
  await expect(page.getByTestId('doc-body').locator('h2#rollback')).toBeInViewport();
});

test('verify, pin and live updates from the API', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/docs/d${nums.runbook}`);
  await expect(page.getByTestId('doc-verification')).toContainText('never verified');
  await page.getByTestId('doc-verify').click();
  await expect(page.getByTestId('doc-verification')).toContainText('verified by owner');
  await expect.poll(async () => (await getDoc(nums.runbook)).verified_by).toBe('owner');
  await page.getByTestId('doc-pin').click();
  await expect(page.getByTestId('doc-pin')).toHaveText('Pin');
  // be-lead retitles it: the page shows it without a reload.
  await api(s.seatTokens['be-lead'], 'PATCH', `/docs/${nums.runbook}`, { title: 'Deploy and rollback procedure' });
  await expect(page.locator('.docpage .page-head h1')).toContainText('Deploy and rollback procedure');
});

test('needs you lists decision drafts and the user activates one', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/needs`);
  const box = page.getByTestId('needs-docs');
  const row = box.locator(`li[data-doc="${nums.decision}"]`);
  await expect(row).toContainText('Use trigram search for archives');
  await row.getByRole('button', { name: 'Activate' }).click();
  await expect(row).toHaveCount(0);
  await expect.poll(async () => (await getDoc(nums.decision)).status).toBe('active');
});

test('at phone width the reader does not cover the drafts and their Activate button', async ({ page }) => {
  const s = state();
  const num = await newDoc(s.seatTokens['be-lead'], {
    title: 'Shard the archive index by month',
    kind: 'decision',
    body: '## Context\nBig index.\n## Decision\nMonthly shards.\n## Alternatives\nOne index.\n## Consequences\nMore files.\n',
    team: 'backend',
  });
  await api(s.seatTokens['be-1'], 'POST', '/messages', { to: ['owner'], subject: 'Shards: which month boundary?', body: 'UTC or local time?' });
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/needs`);
  // The first message is open in the reader below the queue.
  await expect(page.locator('.needs .reader article, .needs .reader .card').first()).toBeVisible();
  const button = page.getByTestId('needs-docs').locator(`li[data-doc="${num}"]`).getByRole('button', { name: 'Activate' });
  await button.scrollIntoViewIfNeeded();
  await expect(button).toBeVisible();
  // Nothing is drawn over the button: the element at its centre is the
  // button itself.
  const onTop = await button.evaluate((b) => {
    const r = b.getBoundingClientRect();
    const el = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
    return !!el && b.contains(el);
  });
  expect(onTop).toBe(true);
  const wide = await page.locator('#main').evaluate((m) => m.scrollWidth - m.clientWidth);
  expect(wide).toBeLessThanOrEqual(1);
  await button.click();
  await expect.poll(async () => (await getDoc(num)).status).toBe('active');
});

test('the thread page lists the documents it cites and marks a comment on a removed section', async ({ page }) => {
  const s = state();
  const lead = s.seatTokens['be-lead'];
  const num = await newDoc(lead, {
    title: 'Archive import notes',
    body: '# Import\n\nRun the importer.\n\n# Old format\n\nThe 2019 files.\n',
    team: 'backend',
  });
  const c = await api<{ id: number; thread_id: number }>(s.userToken, 'POST', `/docs/${num}/comment`, { anchor: 'old-format', body: 'Do we still get these?' });
  await api(lead, 'PUT', `/docs/${num}/body`, { body: '# Import\n\nRun the importer.\n' });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/thread/${c.thread_id}`);
  await expect(page.getByTestId('thread-docs')).toContainText(`d${num}`);
  await expect(page.getByTestId('thread-docs')).toContainText('Archive import notes');
  await expect(page.getByTestId('comment-anchor').first()).toContainText('on a removed section (was: Old format)');
});

test('a comment on a section shows its anchor in the discussion', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/docs/d${nums.runbook}`);
  await page.getByTestId('comment-anchor-select').selectOption('rollback');
  await page.getByPlaceholder('Write a comment').fill('Which tag is the previous one?');
  await page.getByRole('button', { name: 'Comment', exact: true }).click();
  const disc = page.getByTestId('doc-discussion');
  await expect(disc).toContainText('Which tag is the previous one?');
  await expect(disc.getByTestId('comment-anchor').first()).toContainText('#rollback');
});

test('the seat Handover tab shows the note, its reads and requests, and writes one', async ({ page }) => {
  const s = state();
  await api(s.seatTokens['be-2'], 'PUT', '/seats/be-2/handover', {
    text: 'State: on track, index built\nSummary: The trigram index is in; the API is next.\nNext:\n1. cargo test -p store search\nWaiting on: nothing\nTraps: The CJK test is slow.\nVerify: cargo test passes\nPointers: t1\n',
    session: 'sess-handover-1',
  });
  await api(s.seatTokens['be-2'], 'POST', '/seats/be-2/handover/read', { session: 'sess-handover-2' });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/org/be-2?tab=handover`);
  const tab = page.getByTestId('seat-handover');
  await expect(tab.locator('[data-field="state"]')).toContainText('on track, index built');
  await expect(tab.locator('[data-field="next"]')).toContainText('cargo test -p store search');
  await expect(tab.getByTestId('handover-read')).toContainText('sess-handover');
  await tab.getByTestId('handover-request').click();
  await expect(tab.getByTestId('handover-requested')).toContainText('owner asked for a note');
  // The user writes the next note in the seat's name.
  await tab.getByTestId('handover-edit').click();
  const text = tab.getByTestId('handover-text');
  await expect(text).toHaveValue(/## State\non track, index built/);
  await text.fill((await text.inputValue()).replace('on track, index built', 'blocked, waiting for review'));
  await tab.getByRole('button', { name: 'Save note' }).click();
  await expect(tab.locator('[data-field="state"]')).toContainText('blocked, waiting for review');
  await expect(tab.getByTestId('handover-meta')).toContainText('written by owner');
  await expect(tab.getByTestId('handover-read')).toContainText('No session has read');
  // The older revision is still there.
  await tab.getByTestId('handover-rev').selectOption('1');
  await expect(tab.locator('[data-field="state"]')).toContainText('on track, index built');
  await shot(page, 'web-seat-handover');
});

test('the seat Worklog tab lists entries with filters and counts', async ({ page }) => {
  const s = state();
  const at = (min: number) => new Date(Date.now() - min * 60_000).toISOString();
  await api(s.nodeToken, 'POST', '/worklog', {
    entries: [
      { seat: 'fe-2', session: 'sess-w', at: at(30), kind: 'session_start', text: 'startup' },
      { seat: 'fe-2', session: 'sess-w', at: at(20), kind: 'tool', text: 'Bash: npm test', task: nums.task },
      { seat: 'fe-2', session: 'sess-w', at: at(10), kind: 'tool', text: 'Edit: src/search.ts' },
      { seat: 'fe-2', session: 'sess-w', at: at(5), kind: 'commit', text: 'abc1234 Add the search page' },
    ],
  });
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/org/fe-2?tab=worklog`);
  const tab = page.getByTestId('seat-worklog');
  await expect(tab.locator('li[data-kind="commit"]')).toContainText('abc1234 Add the search page');
  await expect(tab.getByTestId('worklog-summary')).toContainText('tool 2');
  await tab.getByTestId('worklog-kind').selectOption('tool');
  await expect(tab.locator('li[data-kind]')).toHaveCount(2);
  await expect(tab.locator('li[data-kind="commit"]')).toHaveCount(0);
  await tab.getByTestId('worklog-kind').selectOption('');
  await tab.getByTestId('worklog-task').fill(`t${nums.task}`);
  await expect(tab.locator('li[data-kind]')).toHaveCount(1);
  await expect(tab.locator('li[data-kind]')).toContainText('Bash: npm test');
});

test('the document pages fit a phone in the dark theme', async ({ page }) => {
  const s = state();
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page, s.userToken, { lang: 'zh', theme: 'dark' });
  for (const [route, name] of [
    [`#/docs/d${nums.runbook}`, 'web-doc-viewer-phone'],
    ['#/docs?team=backend', 'web-docs-landing-phone'],
    ['#/docs/new', 'web-doc-new-phone'],
  ] as const) {
    await page.goto(`${s.base}/${route}`);
    await expect(page.locator('.page-head h1').first()).toBeVisible();
    await page.waitForTimeout(300);
    const wide = await page.locator('#main').evaluate((m) => m.scrollWidth - m.clientWidth);
    expect(wide, route).toBeLessThanOrEqual(1);
    await shot(page, name);
  }
});
