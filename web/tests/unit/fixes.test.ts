// Sent list delivery states, one seat status for row and panel, the
// highlighted sidebar entry of a thread, and the data notice.

import { afterEach, describe, expect, it } from 'vitest';
import { flushSync, mount, unmount, type Component } from 'svelte';
import MailList from '../../src/components/MailList.svelte';
import DataNotice from '../../src/components/DataNotice.svelte';
import TreeRow from '../../src/components/TreeRow.svelte';
import { deliverySummary } from '../../src/lib/delivery';
import { fromSent } from '../../src/lib/mailrows';
import { navSection, sectionOf, threadFolder } from '../../src/lib/nav';
import { seatStatus, stateLongKey } from '../../src/lib/presence';
import { parseHash } from '../../src/lib/router.svelte';
import { dictionaries } from '../../src/lib/i18n';
import { ui } from '../../src/lib/ui.svelte';
import type { MessageHeader, Seat } from '../../src/lib/types';

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

const header = (id: number, from = 'owner', to = ['root']): MessageHeader => ({
  id,
  thread_id: id,
  from,
  source: 'user',
  to,
  cc: [],
  subject: `Message ${id}`,
  priority: 'normal',
  ack_required: false,
  lines: 1,
  created_at: '2026-10-04T10:00:00Z',
});

describe('sent list delivery states', () => {
  it('summarizes the states most advanced first, without empty ones', () => {
    expect(deliverySummary({ queued: 1, notified: 0, read: 2, acked: 0 })).toEqual([
      { state: 'read', n: 2 },
      { state: 'queued', n: 1 },
    ]);
    expect(deliverySummary({ queued: 0, notified: 0, read: 0, acked: 3 })).toEqual([{ state: 'acked', n: 3 }]);
    expect(deliverySummary({ queued: 0, notified: 0, read: 0, acked: 0 })).toEqual([]);
  });

  it('joins the counts to the rows of the page', () => {
    const rows = fromSent({
      messages: [header(2), header(1)],
      delivery: [{ message_id: 2, queued: 1, notified: 0, read: 2, acked: 0 }],
    });
    expect(rows[0].delivery?.read).toBe(2);
    expect(rows[1].delivery).toBeUndefined();
    // An older server sends no counts.
    expect(fromSent({ messages: [header(3)] })[0].delivery).toBeUndefined();
  });

  it('shows the summary in the list', () => {
    ui.lang = 'en';
    const rows = fromSent({ messages: [header(2)], delivery: [{ message_id: 2, queued: 1, notified: 0, read: 2, acked: 0 }] });
    const el = render(MailList, { rows, showTo: true });
    const sum = el.querySelector('[data-testid="delivery-summary"]')!;
    expect(sum.textContent?.replace(/\s+/g, ' ').trim()).toBe('2 read·1 queued');
    expect(sum.getAttribute('title')).toContain('Delivery state');
    ui.lang = 'zh';
    flushSync();
    expect(el.querySelector('[data-testid="delivery-summary"]')!.textContent).toContain('2 已读');
    ui.lang = 'en';
  });
});

describe('seat status', () => {
  const now = Date.parse('2026-10-04T12:00:00Z');
  const seat = (presence?: Seat['presence']): Seat => ({ name: 'root', kind: 'agent', created_at: '2026-10-01T00:00:00Z', presence });
  const fresh = { since: '2026-10-04T11:59:40Z', updated_at: '2026-10-04T11:59:50Z' };

  it('says starting for a seat offline at the trust prompt, as its row does', () => {
    ui.lang = 'en';
    const s = seat({ state: 'offline', attention: 'trust-prompt', ...fresh });
    const st = seatStatus(s, now);
    expect(st).toMatchObject({ state: 'offline', starting: true, failing: false, tag: 'trust-prompt' });
    expect(stateLongKey(st)).toBe('state.starting.long');
    const el = render(TreeRow, {
      row: { name: 'root', depth: 0, hasChildren: false, expanded: false, context: false },
      seat: s,
      size: 0,
      now,
    });
    expect(el.querySelector('[data-starting]')?.textContent).toContain('starting');
  });

  it('says offline without a report, and drops a stale one', () => {
    expect(stateLongKey(seatStatus(seat(), now))).toBe('state.offline.long');
    const stale = seat({ state: 'offline', attention: 'trust-prompt', since: '2026-10-04T11:00:00Z', updated_at: '2026-10-04T11:00:00Z' });
    expect(seatStatus(stale, now)).toMatchObject({ starting: false, presence: undefined });
  });

  it('flags failing turns', () => {
    const s = seat({ state: 'idle', ...fresh, turn_failed: { reason: 'x', at: fresh.since, count: 3, retrying: false } });
    expect(seatStatus(s, now)).toMatchObject({ state: 'idle', failing: true, tag: 'turn-failed' });
  });
});

describe('sidebar entry of a thread', () => {
  it('maps the inbox tabs to Inbox and Sent', () => {
    expect(sectionOf(parseHash('#/inbox'))).toBe('inbox');
    expect(sectionOf(parseHash('#/inbox?tab=sent'))).toBe('sent');
    expect(sectionOf(parseHash(''))).toBe('org');
    expect(sectionOf(parseHash('#/thread/4?m=5'))).toBe('thread');
  });

  it('keeps the list a thread was opened from', () => {
    expect(navSection('thread', 'sent', 'inbox')).toBe('sent');
    expect(navSection('thread', 'needs', null)).toBe('needs');
    expect(navSection('inbox', 'sent', null)).toBe('inbox');
  });

  it('decides by sender when the thread was opened by its URL or after sending', () => {
    const msgs = [header(1, 'owner', ['root']), header(2, 'root', ['owner'])];
    expect(threadFolder(msgs, 'owner')).toBe('sent');
    expect(threadFolder(msgs, 'owner', 2)).toBe('inbox');
    // Between two agents: unclear, no entry.
    expect(threadFolder([header(3, 'be-lead', ['be-1'])], 'owner')).toBeNull();
    expect(threadFolder(msgs, undefined)).toBeNull();
    expect(navSection('thread', null, 'sent')).toBe('sent');
    expect(navSection('thread', 'compose', 'sent')).toBe('sent');
    expect(navSection('thread', null, null)).toBeNull();
  });
});

describe('data notice', () => {
  it('says what a person can delete', () => {
    for (const lang of ['en', 'zh'] as const) {
      ui.lang = lang;
      const el = render(DataNotice, {});
      const text = el.textContent ?? '';
      expect(text).toContain(dictionaries[lang]['data.deleteMail']);
      expect(text).toContain(dictionaries[lang]['data.noRetention']);
    }
    ui.lang = 'en';
    expect(dictionaries.en['data.noRetention']).toContain('until you delete it');
    expect(dictionaries.en['data.deleteMail']).toContain('Mail exchanged with orgs that still exist cannot be deleted');
  });
});
