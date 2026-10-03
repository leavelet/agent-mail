import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { scrollToRow, virtualWindow } from '../../src/lib/virtual';
import { effectiveState, freshPresence, needsPerson, nodeOpBlock, stateAge } from '../../src/lib/presence';
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
