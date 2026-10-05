// Screenshots of every main page in light/dark × English/Chinese, plus the
// org page of a 1,000-seat org, into .e2e/screenshots/ (or $AMAIL_SCREENSHOTS).

import fs from 'node:fs';
import path from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import { api, login, pushPresence, SCREENSHOTS, state } from './env';

test.describe.configure({ mode: 'serial' });

const combos = [
  { theme: 'light', lang: 'en' },
  { theme: 'dark', lang: 'en' },
  { theme: 'light', lang: 'zh' },
  { theme: 'dark', lang: 'zh' },
] as const;

fs.mkdirSync(SCREENSHOTS, { recursive: true });

async function shot(page: Page, name: string, theme: string, lang: string) {
  await page.waitForTimeout(250);
  await page.screenshot({ path: path.join(SCREENSHOTS, `${name}-${theme}-${lang}.png`) });
}

async function kickoffThread(): Promise<{ thread: number; id: number }> {
  const s = state();
  const r = await api<{ messages: { id: number; thread_id: number; subject: string }[] }>(s.userToken, 'GET', '/archive/messages?from=owner&limit=100');
  const m = r.messages.find((x) => x.subject.startsWith('Kickoff'))!;
  return { thread: m.thread_id, id: m.id };
}

for (const { theme, lang } of combos) {
  test(`pages ${theme} ${lang}`, async ({ browser }) => {
    const s = state();
    await pushPresence();
    const ctx = await browser.newContext({ colorScheme: theme, locale: lang === 'zh' ? 'zh-CN' : 'en-US', viewport: { width: 1440, height: 900 } });
    const page = await ctx.newPage();
    await page.addInitScript(([l, t]) => {
      localStorage.setItem('amail.lang', l);
      localStorage.setItem('amail.theme', t);
    }, [lang, theme]);

    await page.goto(`${s.base}/`);
    await page.locator('#token').waitFor();
    await shot(page, 'login', theme, lang);

    await login(page, s.userToken, { lang, theme });
    await page.goto(`${s.base}/#/org`);
    await expect(page.locator('.trow[data-seat="root"]')).toBeVisible();
    await shot(page, 'org', theme, lang);

    await page.goto(`${s.base}/#/org/be-1`);
    if (s.node) await expect(page.getByTestId('transcript-log')).toContainText('trigram', { timeout: 20_000 });
    await shot(page, 'seat-detail-mirror', theme, lang);

    await page.goto(`${s.base}/#/org/fe-lead`);
    await expect(page.locator('.detail .alert')).toBeVisible();
    await shot(page, 'seat-detail-needs-person', theme, lang);

    await page.goto(`${s.base}/#/needs`);
    await expect(page.locator('.item').first()).toBeVisible();
    await shot(page, 'needs-you', theme, lang);

    await page.goto(`${s.base}/#/inbox`);
    await expect(page.locator('.mrow').first()).toBeVisible();
    await shot(page, 'inbox', theme, lang);

    const k = await kickoffThread();
    await page.goto(`${s.base}/#/thread/${k.thread}?m=${k.id}`);
    const art = page.locator(`article[data-msg="${k.id}"]`);
    await art.locator('footer button').last().click();
    await expect(art.locator('[data-testid="delivery"] tbody tr').first()).toBeVisible();
    await shot(page, 'thread-delivery', theme, lang);

    await page.goto(`${s.base}/#/archive`);
    await expect(page.locator('.results .mrow').first()).toBeVisible();
    await shot(page, 'archive-timeline', theme, lang);

    await page.goto(`${s.base}/#/archive?q=${encodeURIComponent('迁移方案')}`);
    await expect(page.locator('.results .mrow').first()).toBeVisible();
    await shot(page, 'archive-search-chinese', theme, lang);

    await page.goto(`${s.base}/#/archive?q=${encodeURIComponent('迁移')}`);
    await expect(page.locator('.results[data-mode="scan"] .mrow').first()).toBeVisible();
    await shot(page, 'archive-search-short-scan', theme, lang);

    await page.goto(`${s.base}/#/compose?to=be-lead`);
    await page.locator('#c-subj').fill(lang === 'zh' ? '接口改为 RFC 3339 时间' : 'Switch the API to RFC 3339 times');
    await page.locator('#c-body').fill(lang === 'zh' ? '所有时间字段改用 RFC 3339。\n改完后回复我。' : 'All time fields use RFC 3339.\nReply when done.');
    await page.locator('#c-cc').fill('fe-');
    await page.locator('#c-cc').focus();
    await shot(page, 'compose', theme, lang);

    // Other orgs, openings, teams, masked recipients.
    await page.goto(`${s.base}/#/orgs`);
    await expect(page.locator(`[data-testid="links-table"] tr[data-org="${s.peer.org}"]`)).toBeVisible();
    await shot(page, 'others', theme, lang);

    await page.goto(`${s.base}/#/orgs/${s.peer.org}`);
    await expect(page.getByTestId('open-to-you').locator('tr[data-card]').first()).toBeVisible();
    await page.getByTestId('you-open').locator('select').selectOption('be-lead');
    await page.getByTestId('you-open').getByTestId('open-add').click();
    await expect(page.getByTestId('preview-lines')).toBeVisible();
    await shot(page, 'peer-org', theme, lang);

    await page.goto(`${s.base}/#/admin/openings`);
    await page.getByTestId('grant-target').selectOption('backend');
    await page.getByTestId('open-add').click();
    await expect(page.getByTestId('preview-lines')).toBeVisible();
    await shot(page, 'openings-preview', theme, lang);

    await page.goto(`${s.base}/#/teams/backend`);
    await expect(page.getByTestId('team-form')).toBeVisible();
    await shot(page, 'teams', theme, lang);

    await page.goto(`${s.base}/#/org/be-lead`);
    const editor = page.locator('[data-testid="openings-editor"][data-target="be-lead"]');
    await editor.getByRole('combobox').selectOption(s.peer.org);
    await editor.getByTestId('open-add').click();
    await expect(editor.getByTestId('preview-lines')).toBeVisible();
    await shot(page, 'seat-openings', theme, lang);

    const masked = await api<{ entries: { message: { id: number; thread_id: number; subject: string } }[] }>(s.userToken, 'GET', '/inbox?seat=owner&limit=200');
    const mm = masked.entries.map((e) => e.message).find((x) => x.subject === 'Billing API questions')!;
    await page.goto(`${s.base}/#/thread/${mm.thread_id}?m=${mm.id}`);
    await expect(page.getByTestId('to-hidden')).toBeVisible();
    await shot(page, 'thread-masked', theme, lang);

    for (const a of ['seats', 'lists', 'openings', 'tokens', 'settings']) {
      await page.goto(`${s.base}/#/admin/${a}`);
      await page.waitForTimeout(300);
      await shot(page, `admin-${a}`, theme, lang);
    }

    await page.goto(`${s.base}/#/inbox`);
    await page.keyboard.press('?');
    await expect(page.getByRole('dialog')).toBeVisible();
    await shot(page, 'keyboard-help', theme, lang);
    await ctx.close();
  });
}

test('1,000-seat org', async ({ browser }) => {
  const s = state();
  const big = s.big!;
  const seats = await api<{ name: string; kind: string }[]>(big.userToken, 'GET', '/seats');
  const agents = seats.filter((x) => x.kind === 'agent');
  expect(agents.length).toBe(1000);
  const states = ['busy', 'busy', 'busy', 'idle', 'idle', 'dormant', 'awaiting', 'offline'] as const;
  const updates = agents.map((a, i) => ({
    seat: a.name,
    state: states[(i * 7) % states.length],
    harness: 'claude',
    since: new Date(Date.now() - ((i * 37) % 3600) * 1000).toISOString(),
    ...(i % 97 === 5 ? { attention: 'trust-prompt', attention_hint: `answer the trust prompt (amail attach ${a.name})` } : {}),
    ...(i % 131 === 7 ? { wake_failed: true } : {}),
  }));
  await api(big.nodeToken, 'POST', '/presence', { node: 'rack-a', seats: updates });

  for (const { theme, lang } of [combos[0], combos[3]]) {
    const ctx = await browser.newContext({ colorScheme: theme, viewport: { width: 1440, height: 900 } });
    const page = await ctx.newPage();
    await login(page, big.userToken, { lang, theme });
    await page.goto(`${s.base}/#/org`);
    await expect(page.locator('.trow[data-seat="chief"]')).toBeVisible();
    // Only a window of rows is in the DOM.
    const rendered = await page.locator('.trow').count();
    expect(rendered).toBeLessThan(120);
    await shot(page, 'org-1000-seats', theme, lang);

    // Scroll to the end and collapse everything: both stay fast.
    const t = await page.evaluate(async () => {
      const sc = document.querySelector('.scroller') as HTMLElement;
      const t0 = performance.now();
      for (let i = 0; i < 20; i++) {
        sc.scrollTop = (sc.scrollHeight * i) / 20;
        await new Promise((r) => requestAnimationFrame(() => r(null)));
      }
      return performance.now() - t0;
    });
    console.log(`1,000 seats: 20 scroll steps with repaint in ${t.toFixed(0)} ms`);
    await shot(page, 'org-1000-seats-scrolled', theme, lang);
    const c0 = Date.now();
    await page.getByRole('button', { name: /^(Collapse all|全部折叠)$/ }).click();
    await expect(page.locator('.trow')).toHaveCount(2);
    console.log(`1,000 seats: collapse all in ${Date.now() - c0} ms`);
    await page.locator('.trow[data-seat="chief"] .caret').click();
    await page.locator('.scroller').evaluate((el) => (el.scrollTop = 0));
    await expect(page.locator('.trow[data-seat="team00-lead"]')).toBeVisible();
    await expect(page.locator('.trow[data-seat="team00-w1"]')).toHaveCount(0);
    await shot(page, 'org-1000-seats-collapsed', theme, lang);
    await page.getByRole('button', { name: /needs a person|需要人工处理/ }).first().click();
    await shot(page, 'org-1000-seats-filter-needs-person', theme, lang);
    await ctx.close();
  }
});
