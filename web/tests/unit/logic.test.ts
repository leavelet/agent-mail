import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { scrollToRow, virtualWindow } from '../../src/lib/virtual';
import {
  attentionTag,
  effectiveState,
  focusUntil,
  freshPresence,
  needsPerson,
  nodeOpBlock,
  startingAtPrompt,
  stateAge,
  terminalHref,
  terminalSession,
} from '../../src/lib/presence';
import { noteKey } from '../../src/lib/receipt';
import { Coalescer } from '../../src/lib/coalesce';
import { attentionKey, seatsWaitingForUser, sortAttention } from '../../src/lib/attention';
import { deliveryState, deliveryStep, viaLabel } from '../../src/lib/delivery';
import { duration } from '../../src/lib/time';
import type { AttentionItem, MessageHeader, Seat } from '../../src/lib/types';

describe('virtual window', () => {
  it('renders only the visible rows plus overscan', () => {
    const w = virtualWindow(3000, 600, 30, 1000, 5);
    expect(w.start).toBe(95);
    expect(w.end).toBe(126);
    expect(w.padTop).toBe(95 * 30);
    expect(w.padBottom).toBe((1000 - 126) * 30);
    expect(virtualWindow(0, 600, 30, 10)).toMatchObject({ start: 0, end: 10, padTop: 0, padBottom: 0 });
    expect(virtualWindow(0, 600, 30, 0)).toMatchObject({ start: 0, end: 0 });
  });
  it('scrolls a row into view', () => {
    expect(scrollToRow(10, 0, 300, 30)).toBe(30);
    expect(scrollToRow(2, 300, 300, 30)).toBe(60);
    expect(scrollToRow(12, 300, 300, 30)).toBe(300);
  });
});

const now = Date.parse('2026-10-04T12:00:00Z');
function seat(p?: Partial<Seat['presence']>, kind: 'agent' | 'user' = 'agent'): Seat {
  return {
    name: 'a',
    kind,
    created_at: '2026-10-01T00:00:00Z',
    presence: p
      ? { state: 'busy', since: '2026-10-04T11:58:00Z', updated_at: '2026-10-04T11:59:30Z', ...p }
      : undefined,
  };
}

describe('presence', () => {
  it('treats presence older than 90 s as offline', () => {
    expect(effectiveState(seat({}), now)).toBe('busy');
    expect(effectiveState(seat({ updated_at: '2026-10-04T11:58:00Z' }), now)).toBe('offline');
    expect(effectiveState(seat(), now)).toBe('offline');
    expect(freshPresence(undefined, now)).toBeUndefined();
    expect(stateAge(seat({}), now)).toBe(120_000);
  });
  it('flags seats that need a person and blocks node operations with a reason', () => {
    expect(needsPerson(seat({ attention: 'login' }), now)).toBe(true);
    expect(needsPerson(seat({ wake_failed: true }), now)).toBe(true);
    expect(needsPerson(seat({}), now)).toBe(false);
    expect(nodeOpBlock(seat({ node_connected: true }), now)).toBeNull();
    expect(nodeOpBlock(seat({ node_connected: false }), now)).toBe('noNode');
    expect(nodeOpBlock(seat(), now)).toBe('noPresence');
    expect(nodeOpBlock(seat({}, 'user'), now)).toBe('user');
  });
  it('shows focus only while it lasts', () => {
    const later = new Date(now + 600_000).toISOString();
    const earlier = new Date(now - 1).toISOString();
    expect(focusUntil({ ...seat({}), focus_until: later }, now)).toBe(later);
    expect(focusUntil({ ...seat({}), focus_until: earlier }, now)).toBeUndefined();
    expect(focusUntil(seat({}), now)).toBeUndefined();
  });
});

describe('coalescer', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());
  it('merges keys into at most one flush per interval', () => {
    const flushes: string[][] = [];
    const c = new Coalescer<string>(1000, (k) => flushes.push([...k].sort()), () => Date.now());
    c.add('a');
    c.add('b');
    c.add('a');
    expect(flushes).toEqual([]);
    vi.advanceTimersByTime(0);
    expect(flushes).toEqual([['a', 'b']]);
    c.add('c');
    vi.advanceTimersByTime(500);
    c.add('d');
    expect(flushes.length).toBe(1);
    vi.advanceTimersByTime(500);
    expect(flushes).toEqual([['a', 'b'], ['c', 'd']]);
    c.add('e');
    c.cancel();
    vi.advanceTimersByTime(5000);
    expect(flushes.length).toBe(2);
  });
});

function msg(id: number, p: Partial<MessageHeader> = {}): MessageHeader {
  return {
    id,
    thread_id: id,
    from: 'w1',
    source: 'agent',
    to: ['owner'],
    cc: [],
    subject: `s${id}`,
    priority: 'normal',
    ack_required: false,
    lines: 1,
    created_at: '2026-10-04T10:00:00Z',
    ...p,
  };
}

describe('attention', () => {
  const items: AttentionItem[] = [
    { kind: 'reply', since: '2026-10-04T09:00:00Z', seat: 'w1', message: msg(1) },
    { kind: 'seat', since: '2026-10-04T11:00:00Z', seat: 'w2', attention: 'login' },
    { kind: 'ack', since: '2026-10-04T10:00:00Z', seat: 'w3', message: msg(2, { priority: 'urgent', ack_required: true }) },
    { kind: 'reply', since: '2026-10-04T08:00:00Z', seat: 'x@other', message: msg(3, { source: 'external' }) },
  ];
  it('sorts urgent mail first, then seats, then other mail oldest first', () => {
    expect(sortAttention(items).map(attentionKey)).toEqual(['msg:2', 'seat:w2', 'msg:3', 'msg:1']);
  });
  it('marks local seats with mail waiting for the user', () => {
    expect([...seatsWaitingForUser(items)].sort()).toEqual(['w1', 'w3']);
  });
});

describe('delivery labels', () => {
  it('derives state from timestamps', () => {
    expect(deliveryState({})).toBe('queued');
    expect(deliveryState({ notified_at: 'x' })).toBe('notified');
    expect(deliveryState({ notified_at: 'x', read_at: 'y' })).toBe('read');
    expect(deliveryState({ read_at: 'y', acked_at: 'z' })).toBe('acked');
    expect(deliveryStep('read')).toBe(2);
    expect(viaLabel('hook:PostToolUse')).toBe('PostToolUse hook');
    expect(viaLabel('cli')).toBe('cli');
    expect(viaLabel(undefined)).toBe('');
  });
});

describe('durations', () => {
  it('formats compactly in both languages', () => {
    expect(duration(45_000)).toBe('45s');
    expect(duration(12 * 60_000)).toBe('12m');
    expect(duration(3 * 3600_000 + 5 * 60_000)).toBe('3h 5m');
    expect(duration(2 * 86400_000)).toBe('2d');
    expect(duration(3 * 3600_000 + 5 * 60_000, 'zh')).toBe('3小时5分');
  });
});

import { reachedBy } from '../../src/lib/delivery';

describe('how mail reached a seat', () => {
  const isList = (a: string) => a === 'backend' || a === 'leads';
  it('names the field, and the list when the seat is not named itself', () => {
    expect(reachedBy('a', 'to', { to: ['a'], cc: [] }, isList)).toEqual({ cls: 'to' });
    expect(reachedBy('a', 'cc', { to: ['b'], cc: ['a'] }, isList)).toEqual({ cls: 'cc' });
    expect(reachedBy('a', 'to', { to: ['b', 'backend'], cc: ['leads'] }, isList)).toEqual({ cls: 'to', via: 'backend' });
    expect(reachedBy('a', 'cc', { to: ['backend'], cc: ['leads'] }, isList)).toEqual({ cls: 'cc', via: 'leads' });
  });
  it('shows old list deliveries like cc', () => {
    expect(reachedBy('a', 'list', { to: ['backend'], cc: [] }, isList)).toEqual({ cls: 'cc', via: 'backend' });
  });
});

describe('terminal links', () => {
  it('keeps only http and https URLs', () => {
    expect(terminalHref('https://host.example/t/a')).toBe('https://host.example/t/a');
    expect(terminalHref('http://127.0.0.1:9/x')).toBe('http://127.0.0.1:9/x');
    expect(terminalHref('javascript:alert(1)')).toBeUndefined();
    expect(terminalHref('data:text/html,x')).toBeUndefined();
    expect(terminalHref('not a url')).toBeUndefined();
    expect(terminalHref(undefined)).toBeUndefined();
  });
  it('names the group session the link opens', () => {
    expect(terminalSession('http://127.0.0.1:18692/amail-m10acc-app')).toBe('amail-m10acc-app');
    expect(terminalSession('https://h.example/term/amail-o-main/')).toBe('amail-o-main');
    expect(terminalSession('not a url')).toBe('');
  });
});

describe('turn failures and prompts', () => {
  const now = Date.parse('2026-10-04T12:00:00Z');
  const tf = { reason: 'boom', at: '2026-10-04T11:59:00Z', count: 1, retrying: true };
  const base = { state: 'idle' as const, since: '2026-10-04T11:00:00Z', updated_at: '2026-10-04T11:59:30Z' };
  it('counts a failing seat as needing a person', () => {
    const seat: Seat = { name: 'a', kind: 'agent', created_at: 'x', presence: { ...base, turn_failed: tf } };
    expect(needsPerson(seat, now)).toBe(true);
    expect(attentionTag(seat.presence)).toBe('turn-failed');
    expect(attentionTag({ ...base, attention: 'login', turn_failed: tf })).toBe('login');
    expect(attentionTag({ ...base, wake_failed: true })).toBe('wake-failed');
    expect(attentionTag(base)).toBeUndefined();
  });
  it('calls an offline seat at a prompt starting', () => {
    expect(startingAtPrompt({ ...base, state: 'offline', attention: 'trust-prompt' })).toBe(true);
    expect(startingAtPrompt({ ...base, state: 'idle', attention: 'trust-prompt' })).toBe(false);
    expect(startingAtPrompt({ ...base, state: 'offline', attention: 'failed' })).toBe(false);
    expect(startingAtPrompt(undefined)).toBe(false);
  });
});

describe('send receipt notes', () => {
  it('match the meaning of the CLI', () => {
    expect(noteKey({ seat: 'a', state: 'dormant', wakes: true }).key).toBe('compose.note.dormantWakes');
    expect(noteKey({ seat: 'a', state: 'dormant', wakes: false }).key).toBe('compose.note.dormant');
    expect(noteKey({ seat: 'a', state: 'focus', wakes: false, until: 'T' })).toEqual({ key: 'compose.note.focus', params: { until: 'T' } });
    expect(noteKey({ seat: 'a', state: 'turn_failed', wakes: true }).key).toBe('compose.note.turnFailed');
    expect(noteKey({ seat: 'a', state: 'new_state', wakes: false })).toEqual({ key: 'compose.note.other', params: { state: 'new_state' } });
  });
});
