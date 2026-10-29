// The Inbox's check-box selection and the plans of its bulk read actions.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount, type Component } from 'svelte';
import MailList from '../../src/components/MailList.svelte';
import {
  CONFIRM_ABOVE,
  headerState,
  markAllBefore,
  readable,
  selectedRows,
  toggleAll,
  toggled,
  unreadable,
  withRead,
} from '../../src/lib/selection';
import { fromEntry } from '../../src/lib/mailrows';
import { ui } from '../../src/lib/ui.svelte';
import type { Delivery, InboxEntry } from '../../src/lib/types';

// The mail actions refresh the store's counts; here only their requests count.
const mailChanged = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock('../../src/lib/store.svelte', () => ({ store: { mailChanged } }));

const entry = (id: number, seq: number, d: Partial<Delivery> = {}): InboxEntry => ({
  delivery: { seat: 'owner', seq, mod_seq: seq, class: 'to', mode: 'immediate', ...d },
  message: {
    id,
    thread_id: id,
    from: 'root',
    source: 'agent',
    to: ['owner'],
    cc: [],
    subject: `Message ${id}`,
    priority: 'normal',
    ack_required: false,
    lines: 1,
    created_at: '2026-10-04T10:00:00Z',
  },
});

const T = '2026-10-04T11:00:00Z';
const entries = [
  entry(10, 1), // unread
  entry(11, 2, { read_at: T, notified_at: T }), // read
  entry(12, 3, { read_at: T, acked_at: T, notified_at: T }), // acknowledged
  entry(13, 4, { withdrawn_at: T, mode: 'archive' }), // withdrawn
  entry(14, 7), // unread
];

describe('selection', () => {
  it('toggles one id without changing the old set', () => {
    const a = new Set<number>();
    const b = toggled(a, 3);
    expect([...b]).toEqual([3]);
    expect(a.size).toBe(0);
    expect(toggled(b, 3).size).toBe(0);
    expect([...toggled(b, 3, true)]).toEqual([3]);
    expect(toggled(b, 4, false)).toEqual(new Set([3]));
  });

  it('keeps the ids that are rows, in list order', () => {
    expect(selectedRows(new Set([5, 1, 9]), [1, 2, 5])).toEqual([1, 5]);
  });

  it('gives the header check box none, some or all', () => {
    const ids = [1, 2, 3];
    expect(headerState(new Set(), ids)).toBe('none');
    expect(headerState(new Set([2]), ids)).toBe('some');
    expect(headerState(new Set([1, 2, 3, 99]), ids)).toBe('all');
    expect(headerState(new Set([99]), ids)).toBe('none');
    expect(headerState(new Set(), [])).toBe('none');
  });

  it('selects every row from the header, and clears when all were selected', () => {
    expect(toggleAll(new Set([2]), [1, 2, 3])).toEqual(new Set([1, 2, 3]));
    expect(toggleAll(new Set([1, 2, 3]), [1, 2, 3])).toEqual(new Set());
  });
});

describe('bulk read plans', () => {
  const all = entries.map((e) => e.message.id);

  it('marks read only unread deliveries that were not withdrawn', () => {
    expect(readable(entries, all)).toEqual([10, 14]);
    expect(readable(entries, [11, 14, 99])).toEqual([14]);
  });

  it('marks unread only read deliveries that were not acknowledged', () => {
    expect(unreadable(entries, all)).toEqual([11]);
  });

  it('leaves mail newer than the loaded list out of "mark all read"', () => {
    expect(markAllBefore(entries)).toBe(8);
    expect(markAllBefore([])).toBeUndefined();
    expect(CONFIRM_ABOVE).toBe(20);
  });

  it('changes the rows in place as the server would', () => {
    const now = '2026-10-04T12:00:00Z';
    const read = withRead(entries, all, true, now);
    expect(read.map((e) => !!e.delivery.read_at)).toEqual([true, true, true, false, true]);
    expect(read[0].delivery.notified_at).toBe(now);
    // Entries that do not change stay the same objects.
    expect(read[1]).toBe(entries[1]);
    expect(read[3]).toBe(entries[3]);
    const rows = read.map((e) => fromEntry(e));
    expect(rows.map((r) => r.unread)).toEqual([false, false, false, false, false]);

    const unread = withRead(read, all, false, '');
    expect(unread.map((e) => !!e.delivery.read_at)).toEqual([false, false, true, false, false]);
    expect(entries[0].delivery.read_at).toBeUndefined();
  });
});

describe('bulk mail requests', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });
  function fake(fail: (url: string) => boolean = () => false) {
    const calls: { url: string; method: string; body?: unknown }[] = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit) => {
      calls.push({ url, method: init.method ?? 'GET', body: init.body ? JSON.parse(String(init.body)) : undefined });
      if (fail(url)) return new Response(JSON.stringify({ error: { code: 'not_found', message: 'no' } }), { status: 404 });
      return new Response(JSON.stringify(url.endsWith('/inbox/read') ? { marked: 2 } : {}), { status: 200 });
    });
    return calls;
  }
  const posts = (calls: { url: string; method: string }[]) => calls.filter((c) => c.method === 'POST');

  it('marks ids read in one request', async () => {
    const { markReadMany } = await import('../../src/lib/mailops');
    const calls = fake();
    expect(await markReadMany([3, 4])).toBe(2);
    expect(posts(calls)).toEqual([{ url: expect.stringMatching(/\/v1\/inbox\/read$/), method: 'POST', body: { ids: [3, 4] } }]);
    // Nothing to mark: no request.
    expect(await markReadMany([])).toBe(0);
    expect(posts(calls).length).toBe(1);
  });

  it('marks all read below a seq', async () => {
    const { markAllRead } = await import('../../src/lib/mailops');
    const calls = fake();
    await markAllRead(8);
    await markAllRead();
    expect(posts(calls).map((c) => (c as { body?: unknown }).body)).toEqual([{ all: true, before_seq: 8 }, { all: true }]);
  });

  it('marks unread one request per id, and reports a failure after trying all', async () => {
    const { markUnreadMany } = await import('../../src/lib/mailops');
    let calls = fake();
    expect(await markUnreadMany([1, 2, 3], 2)).toBe(3);
    expect(posts(calls).map((c) => c.url.replace(/.*\/v1/, ''))).toEqual(['/messages/1/unread', '/messages/2/unread', '/messages/3/unread']);
    vi.unstubAllGlobals();
    calls = fake((url) => url.includes('/messages/2/'));
    await expect(markUnreadMany([1, 2, 3], 2)).rejects.toThrow('no');
    expect(posts(calls).length).toBe(3);
  });
});

describe('mail list check boxes', () => {
  const mounted: Record<string, unknown>[] = [];
  function render<P extends Record<string, unknown>>(C: Component<P>, props: P): HTMLElement {
    const target = document.createElement('div');
    document.body.appendChild(target);
    mounted.push(mount(C, { target, props }) as Record<string, unknown>);
    flushSync();
    return target;
  }
  afterEach(() => {
    for (const m of mounted.splice(0)) unmount(m);
    document.body.innerHTML = '';
  });

  it('has no check boxes without a selection set', () => {
    const el = render(MailList, { rows: entries.map((e) => fromEntry(e)) });
    expect(el.querySelectorAll('[data-testid="row-check"]').length).toBe(0);
  });

  it('ticks a row without selecting it', () => {
    ui.lang = 'en';
    const oncheck = vi.fn();
    const onselect = vi.fn();
    const el = render(MailList, { rows: entries.map((e) => fromEntry(e)), checked: new Set([11]), oncheck, onselect });
    const boxes = [...el.querySelectorAll<HTMLInputElement>('[data-testid="row-check"]')];
    expect(boxes.length).toBe(entries.length);
    expect(boxes.map((b) => b.checked)).toEqual([false, true, false, false, false]);
    expect(boxes[0].getAttribute('aria-label')).toBe('Select: Message 10');
    expect(el.querySelector('[data-msg="11"]')!.classList.contains('checked')).toBe(true);
    boxes[0].click();
    flushSync();
    expect(oncheck).toHaveBeenCalledTimes(1);
    expect(oncheck.mock.calls[0][0].id).toBe(10);
    expect(oncheck.mock.calls[0][1]).toBe(true);
    expect(onselect).not.toHaveBeenCalled();
    // A click elsewhere on the row selects it.
    (el.querySelector('[data-msg="12"] .subj') as HTMLElement).click();
    expect(onselect).toHaveBeenCalledTimes(1);
  });
});
