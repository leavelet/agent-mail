import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount, tick } from 'svelte';
import * as notify from '../../src/lib/notify';
import { translate } from '../../src/lib/i18n';
import type { AttentionItem } from '../../src/lib/types';
import RegisterPage from '../../src/pages/RegisterPage.svelte';
import MessageCard from '../../src/components/MessageCard.svelte';
import { ui } from '../../src/lib/ui.svelte';

const msg = (id: number, from = 'be-lead', subject = 'Question'): AttentionItem => ({
  kind: 'reply',
  since: '2026-10-04T10:00:00Z',
  seat: from,
  message: {
    id,
    thread_id: id,
    from,
    source: 'agent',
    to: ['owner'],
    cc: [],
    subject,
    priority: 'normal',
    ack_required: false,
    lines: 1,
    created_at: '2026-10-04T10:00:00Z',
  },
});
const seatItem: AttentionItem = { kind: 'seat', since: 'x', seat: 'fe-lead', attention: 'permission', hint: 'answer it in the pane' };
const t = (k: string, p?: Record<string, string>) => translate('en', k as 'notify.ack', p);

describe('notifications', () => {
  it('announces only items that arrive after the first load', () => {
    expect(notify.newItems(null, [msg(1)])).toEqual([]);
    const seen = new Set([notify.itemKey(msg(1))]);
    expect(notify.newItems(seen, [msg(1), msg(2), seatItem]).map(notify.itemKey)).toEqual(['reply:m2', 'seat:sfe-lead:permission']);
  });

  it('describes items', () => {
    expect(notify.describe(msg(3, 'root', 'Merge?'), t)).toEqual({ title: 'root is waiting for your answer', body: 'Merge?' });
    expect(notify.describe({ ...msg(4), kind: 'ack' }, t).title).toBe('be-lead asks you to acknowledge');
    expect(notify.describe(seatItem, t)).toEqual({ title: 'Seat fe-lead needs you', body: 'answer it in the pane' });
  });

  describe('with a browser that has notifications', () => {
    const shown: { title: string; body?: string }[] = [];
    beforeEach(() => {
      shown.length = 0;
      localStorage.clear();
      class FakeNotification {
        static permission: NotificationPermission = 'granted';
        static requestPermission = vi.fn(async () => 'granted' as NotificationPermission);
        onclick: (() => void) | null = null;
        constructor(title: string, opts?: { body?: string }) {
          shown.push({ title, body: opts?.body });
        }
        close() {}
      }
      vi.stubGlobal('Notification', FakeNotification);
    });
    afterEach(() => vi.unstubAllGlobals());

    it('is off until the user turns it on, and silent while the page is in front', async () => {
      expect(notify.enabled()).toBe(false);
      expect(notify.announce([msg(5)], t, () => {})).toBe(0);
      expect(await notify.requestPermission()).toBe(true);
      notify.setEnabled(true);
      expect(notify.enabled()).toBe(true);
      const focus = vi.spyOn(document, 'hasFocus').mockReturnValue(true);
      Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true });
      expect(notify.announce([msg(5)], t, () => {})).toBe(0);
      // In the background: one notification per new item.
      Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
      focus.mockReturnValue(false);
      expect(notify.announce([msg(6), seatItem], t, () => {})).toBe(2);
      expect(shown.map((s) => s.title)).toEqual(['be-lead is waiting for your answer', 'Seat fe-lead needs you']);
      notify.setEnabled(false);
      expect(notify.announce([msg(7)], t, () => {})).toBe(0);
    });

    it('does nothing when permission is denied', () => {
      (globalThis.Notification as unknown as { permission: string }).permission = 'denied';
      notify.setEnabled(true);
      Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
      expect(notify.permission()).toBe('denied');
      expect(notify.announce([msg(8)], t, () => {})).toBe(0);
    });
  });
});

describe('RegisterPage', () => {
  const mounted: Record<string, unknown>[] = [];
  afterEach(() => {
    for (const m of mounted.splice(0)) unmount(m);
    document.body.innerHTML = '';
    vi.unstubAllGlobals();
  });

  async function fill(el: HTMLElement, id: string, value: string) {
    const input = el.querySelector<HTMLInputElement>(id)!;
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await tick();
  }

  it('shows the token once registered, and explains refusals', async () => {
    ui.lang = 'en';
    const replies = [
      { status: 403, body: { error: { code: 'invalid_invite', message: 'x' } } },
      { status: 201, body: { org: 'acme', user_seat: 'owner', token: 'amail_' + 'a'.repeat(64) } },
    ];
    const calls: unknown[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (_url: string, init: RequestInit) => {
        calls.push(JSON.parse(String(init.body)));
        const r = replies.shift()!;
        return new Response(JSON.stringify(r.body), { status: r.status });
      }),
    );
    const target = document.createElement('div');
    document.body.appendChild(target);
    mounted.push(mount(RegisterPage, { target }) as Record<string, unknown>);
    flushSync();
    expect(target.textContent).toContain('Your data on this server');
    await fill(target, '#r-invite', 'inv_123');
    await fill(target, '#r-org', 'acme');
    target.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await vi.waitFor(() => expect(target.querySelector('[role=alert]')?.textContent).toContain('not valid'));
    target.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await vi.waitFor(() => expect(target.querySelector('[data-testid=reg-token]')?.textContent).toBe('amail_' + 'a'.repeat(64)));
    expect(target.textContent).toContain('shown only this once');
    expect(target.textContent).toContain('amail login');
    expect(calls[1]).toEqual({ invite: 'inv_123', org: 'acme' });
  });
});

describe('MessageCard', () => {
  it('says when the retention period removed the body', () => {
    ui.lang = 'en';
    const target = document.createElement('div');
    document.body.appendChild(target);
    const m = mount(MessageCard, {
      target,
      props: {
        message: { ...msg(9).message!, expired: true, body: '' },
      },
    });
    flushSync();
    expect(target.textContent).toContain('Body removed');
    unmount(m);
  });
});
