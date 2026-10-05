// Shared state written by global-setup.ts and helpers for the specs.

import fs from 'node:fs';
import path from 'node:path';
import type { Page } from '@playwright/test';

export const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
export const STATE_FILE = path.join(ROOT, '.e2e', 'state.json');
export const SCREENSHOTS = process.env.AMAIL_SCREENSHOTS ?? path.resolve(ROOT, '.e2e/screenshots');

export interface E2eState {
  base: string;
  dir: string;
  org: string;
  userToken: string;
  nodeToken: string;
  seatTokens: Record<string, string>;
  transcript: string;
  node: boolean;
  big?: { org: string; userToken: string; nodeToken: string };
  /** A linked org (`globex`) with seat tokens, and an org that asked acme for a link (`initech`). */
  peer: { org: string; userToken: string; seatTokens: Record<string, string>; inviter: string };
  /** The public-mode server: invite codes and an operator token. */
  pub: { base: string; invites: string[]; opToken: string; db: string };
  pids: number[];
}

export function state(): E2eState {
  return JSON.parse(fs.readFileSync(STATE_FILE, 'utf8')) as E2eState;
}

/** Call the API with a bearer token (playing an agent, a node or the user's CLI). */
export async function api<T = unknown>(token: string, method: string, p: string, body?: unknown): Promise<T> {
  const s = state();
  const r = await fetch(`${s.base}/v1${p}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${p} -> ${r.status}: ${text}`);
  return (text ? JSON.parse(text) : undefined) as T;
}

export async function login(page: Page, token: string, opts: { lang?: 'en' | 'zh'; theme?: 'light' | 'dark' } = {}): Promise<void> {
  const s = state();
  await page.addInitScript(
    ([lang, theme]) => {
      try {
        if (lang) localStorage.setItem('amail.lang', lang);
        if (theme) localStorage.setItem('amail.theme', theme);
      } catch {
        // ignore
      }
    },
    [opts.lang ?? '', opts.theme ?? ''],
  );
  await page.goto(`${s.base}/`);
  await page.locator('#token').fill(token);
  await page.locator('form button[type=submit]').click();
  await page.locator('.shell').waitFor();
}

export const AGENTS: { name: string; parent?: string; harness: string; model: string; group?: string; role: string; description?: string; contact?: boolean }[] = [
  { name: 'root', harness: 'claude', model: 'sonnet', role: 'Overall technical lead: splits the work, reviews, merges into main.', description: 'Technical lead of acme: plans the work and merges it.' },
  { name: 'be-lead', parent: 'root', harness: 'codex', model: 'codex-model', group: 'backend', role: 'Leads the backend group (storage and API).', description: 'Backend questions: storage, search, the HTTP API.', contact: true },
  { name: 'be-1', parent: 'be-lead', harness: 'claude', model: 'sonnet', group: 'backend', role: 'Backend worker: storage layer.' },
  { name: 'be-2', parent: 'be-lead', harness: 'claude', model: 'sonnet', group: 'backend', role: 'Backend worker: HTTP API.' },
  { name: 'fe-lead', parent: 'root', harness: 'claude', model: 'sonnet', group: 'frontend', role: 'Leads the frontend group.' },
  { name: 'fe-1', parent: 'fe-lead', harness: 'claude', model: 'sonnet', group: 'frontend', role: 'Frontend worker: org tree.' },
  { name: 'fe-2', parent: 'fe-lead', harness: 'codex', model: 'codex-model', group: 'frontend', role: 'Frontend worker: mail views.' },
  { name: 'docs', parent: 'root', harness: 'claude', model: 'sonnet', role: 'Writes the user documentation (中文和英文).', description: 'User documentation in English and Chinese.' },
];

/** Fake presence for the seats no real node hosts (it expires after 90 s). */
export async function pushPresence(): Promise<void> {
  const s = state();
  const since = (min: number) => new Date(Date.now() - min * 60_000).toISOString();
  await api(s.nodeToken, 'POST', '/presence', {
    node: 'build-01',
    seats: [
      { seat: 'root', state: 'busy', harness: 'claude', since: since(12) },
      { seat: 'be-lead', state: 'idle', harness: 'codex', since: since(4) },
      { seat: 'be-2', state: 'idle', harness: 'claude', since: since(31), wake_failed: true, attention: 'wake-failed', attention_hint: 'wake-ups are not answered; look at the pane (amail attach be-2)' },
      { seat: 'fe-lead', state: 'awaiting', harness: 'claude', since: since(2), attention: 'permission', attention_hint: 'waiting for a permission decision: answer it in the pane (amail attach fe-lead)' },
      { seat: 'fe-1', state: 'dormant', harness: 'claude', since: since(55) },
      { seat: 'fe-2', state: 'busy', harness: 'codex', since: since(1) },
    ],
  });
}

/** Make `be-lead` acme's only contact for the linked org instead of the
 * user seat (so that mail from the user gets be-lead added in Cc); the
 * returned function restores the default opening of the user seat. */
export async function contactInsteadOfUser(): Promise<() => Promise<void>> {
  const s = state();
  const grants = await api<{ id: number; target: string; audience: string }[]>(s.userToken, 'GET', '/grants');
  const own = grants.find((g) => g.target === 'owner' && g.audience === 'linked');
  if (own) await api(s.userToken, 'DELETE', `/grants/${own.id}`);
  const added = await api<{ grant: { id: number } }>(s.userToken, 'POST', '/grants', { target: 'be-lead', audience: s.peer.org });
  return async () => {
    await api(s.userToken, 'DELETE', `/grants/${added.grant.id}`);
    await api(s.userToken, 'POST', '/grants', { target: 'owner' });
  };
}
