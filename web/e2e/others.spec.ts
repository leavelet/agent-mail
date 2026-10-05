// Other orgs: the page per org, openings with their preview, masked
// recipients and address completion across orgs.

import { expect, test } from '@playwright/test';
import { api, login, state } from './env';

test.describe.configure({ mode: 'serial' });

interface Grant {
  id: number;
  target: string;
  audience: string;
}

async function threadOf(subject: string): Promise<{ thread: number; id: number }> {
  const s = state();
  const r = await api<{ entries: { message: { id: number; thread_id: number; subject: string } }[] }>(s.userToken, 'GET', '/inbox?seat=owner&limit=200');
  const m = r.entries.map((e) => e.message).find((x) => x.subject === subject)!;
  return { thread: m.thread_id, id: m.id };
}

test('the Other orgs list and the page of a linked org say what is open to whom', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/orgs`);
  const row = page.locator(`[data-testid="links-table"] tr[data-org="${s.peer.org}"]`);
  await expect(row).toContainText('active');
  // globex opens help, the eng team and its contact eng-lead (and its user).
  await expect(row.locator('td').nth(3)).toHaveText(/[1-9]/);
  const inv = page.locator(`[data-testid="links-table"] tr[data-org="${s.peer.inviter}"]`);
  await expect(inv).toContainText('invited');
  await expect(inv).toContainText('Initech would like to exchange release notes.');
  await expect(inv.getByRole('button', { name: 'Accept' })).toBeVisible();

  await row.locator('a').first().click();
  await expect(page.getByTestId('org-description')).toHaveText('Builds the billing platform. Ask the front desk first.');
  await expect(page.getByTestId('link-state')).toHaveText('active');
  const open = page.getByTestId('open-to-you');
  await expect(open.locator('tr[data-card="help@globex"]')).toContainText('Front desk of Globex');
  await expect(open.locator('tr[data-card="eng@globex"]')).toContainText('team');
  await expect(open.locator('tr[data-card="eng-lead@globex"]')).toBeVisible();
  await expect(open).not.toContainText('quiet@globex');
  await expect(open).not.toContainText('eng-1@globex');
  // acme's user seat is open to every linked org by default.
  await expect(page.getByTestId('you-open')).toContainText('owner');

  // An org without a link: the card only (here: none), and a request.
  await page.goto(`${s.base}/#/orgs/no-such-org`);
  await expect(page.getByTestId('org-missing')).toBeVisible();
  await expect(page.getByTestId('request-link')).toBeVisible();
});

test('opening and closing a seat shows the preview before anything changes', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/org/docs`);
  const editor = page.locator('[data-testid="openings-editor"][data-target="docs"]');
  await expect(editor).toContainText('Not open to any other org');
  await editor.getByRole('combobox').selectOption('globex');
  await editor.getByTestId('open-add').click();
  const preview = editor.getByTestId('grant-preview');
  await expect(preview.getByTestId('preview-lines')).toContainText('globex will see: docs');
  // Nothing is applied until confirmed.
  expect((await api<Grant[]>(s.userToken, 'GET', '/grants')).some((g) => g.target === 'docs')).toBe(false);
  await preview.getByTestId('preview-apply').click();
  await expect(editor.locator('[data-audience="globex"]')).toBeVisible();
  expect((await api<Grant[]>(s.userToken, 'GET', '/grants')).some((g) => g.target === 'docs' && g.audience === 'globex')).toBe(true);
  // The other org sees it now.
  const seen = await api<{ open_to_you: { address: string }[] }>(s.peer.userToken, 'GET', '/orgs/acme');
  expect(seen.open_to_you.map((c) => c.address)).toContain('docs@acme');

  // Close it again from the table of all openings.
  await page.goto(`${s.base}/#/admin/openings`);
  const row = page.locator('[data-testid="grants-table"] tr[data-target="docs"]');
  await row.getByRole('button', { name: 'Close…' }).click();
  const p2 = page.getByTestId('grant-preview');
  await expect(p2.getByTestId('preview-lines')).toContainText('globex will no longer see: docs');
  await p2.getByRole('button', { name: 'Cancel' }).click();
  await expect(row).toBeVisible();
  await row.getByRole('button', { name: 'Close…' }).click();
  await page.getByTestId('preview-apply').click();
  await expect(row).toHaveCount(0);

  // A change nobody sees: opening a seat to an org that is only asking.
  await page.getByTestId('grant-target').selectOption('fe-1');
  await page.getByTestId('grant-audience').selectOption(s.peer.inviter);
  await page.getByTestId('open-add').click();
  await expect(page.getByTestId('preview-none')).toHaveText('No other org sees anything different.');
  await page.getByRole('button', { name: 'Cancel' }).click();
  expect((await api<Grant[]>(s.userToken, 'GET', '/grants')).some((g) => g.target === 'fe-1')).toBe(false);
});

test('recipients of another org that are not open are shown as a count', async ({ page }) => {
  const s = state();
  const m = await threadOf('Billing API questions');
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/thread/${m.thread}?m=${m.id}`);
  const art = page.locator(`article[data-msg="${m.id}"]`);
  await expect(art.locator('.rcpt')).toContainText('owner');
  await expect(art.getByTestId('to-hidden')).toHaveText('and 1 recipient not shown');
  await expect(art.getByTestId('cc-hidden')).toHaveText('1 recipient not shown');
  await expect(art.locator('.rcpt')).not.toContainText('quiet');
  await expect(art.locator('.rcpt')).not.toContainText('eng-1');

  // The sign-in set English for every load; this later script wins.
  await page.addInitScript(() => localStorage.setItem('amail.lang', 'zh'));
  await page.reload();
  await expect(page.locator(`article[data-msg="${m.id}"]`).getByTestId('to-hidden')).toHaveText('另有 1 位收件人未显示');
});

test('address completion offers only open seats of other orgs and participants of the thread', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/compose`);
  const to = page.locator('#c-to');
  const list = page.locator('#c-to-list');
  await to.fill('eng');
  await expect(list.locator('[data-address="eng-lead@globex"]')).toBeVisible();
  await expect(list.locator('[data-address="eng-1@globex"]')).toHaveCount(0);
  await expect(list.locator('[data-address="eng@globex"]')).toHaveCount(0);
  await to.fill('he');
  await expect(list.locator('[data-address="help@globex"]')).toContainText('open to you');
  await to.fill('qu');
  await expect(list.locator('button')).toHaveCount(0);

  // Answering the thread quiet@globex wrote into: it is offered there.
  const q = await threadOf('Review notes on the export format');
  await page.goto(`${s.base}/#/compose?reply=${q.id}`);
  await expect(to).toHaveValue(/quiet@globex/);
  await page.locator('#c-cc').fill('qu');
  await expect(page.locator('#c-cc-list [data-address="quiet@globex"]')).toContainText('in this thread');

  // A closed seat cannot be written to: the rejection is explained.
  await page.goto(`${s.base}/#/compose`);
  await to.fill('eng-1@globex');
  await page.locator('#c-subj').fill('Hello');
  await page.locator('#c-body').fill('Hi');
  await page.getByRole('button', { name: /^Send/ }).click();
  await expect(page.getByText('unreachable: that org has not opened this seat to yours').first()).toBeVisible();
});

test('the Teams view shows teams, contacts and openings, and edits a team', async ({ page }) => {
  const s = state();
  await login(page, s.userToken, { lang: 'en' });
  await page.goto(`${s.base}/#/teams`);
  await expect(page.locator('a[data-team="backend"]')).toBeVisible();
  await expect(page.locator('a[data-team="frontend"]')).toBeVisible();
  await expect(page.locator('a[data-seat="be-lead"]')).toContainText('contact');
  await expect(page.locator('a[data-seat="docs"]')).toContainText('User documentation');
  await page.locator('a[data-team="frontend"]').click();
  const form = page.getByTestId('team-form');
  await form.locator('textarea').fill('The web interface.');
  await form.locator('label.check', { hasText: 'fe-lead' }).locator('input').check();
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(form).toContainText('Saved.');
  const team = await api<{ description?: string; contacts: string[] }>(s.userToken, 'GET', '/teams/frontend');
  expect(team.description).toBe('The web interface.');
  expect(team.contacts).toEqual(['fe-lead']);
  await expect(page.locator('a[data-team="frontend"]')).toContainText('The web interface.');
});
