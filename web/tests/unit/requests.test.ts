import { describe, expect, it } from 'vitest';
import {
  SNOOZE_MS,
  addSnooze,
  allHaveDefault,
  answerText,
  groupByClass,
  interruptKind,
  isCreationMail,
  isSnoozed,
  parseFilters,
  parseRequestNum,
  parseSnoozes,
  splitAttention,
  subjectRequestTags,
  waitingRequest,
} from '../../src/lib/requests';
import { attentionKey } from '../../src/lib/attention';
import { describe as describeNote, itemKey } from '../../src/lib/notify';
import type { AttentionItem, MessageHeader, Request } from '../../src/lib/types';

function req(num: number, o: Partial<Request> = {}): Request {
  return {
    num,
    kind: 'question',
    seat: 'be-1',
    title: `Question ${num}`,
    options: [
      { key: 'yes', label: 'Ship it' },
      { key: 'no', label: 'Hold it' },
    ],
    class: 'soon',
    state: 'open',
    source: 'cli',
    rev: 1,
    created_at: `2026-10-29T10:0${num % 10}:00Z`,
    ...o,
  };
}

function mail(id: number, subject: string, thread = id): AttentionItem {
  return { kind: 'reply', since: '2026-10-29T10:00:00Z', seat: 'be-1', message: { id, thread_id: thread, subject } as MessageHeader };
}

describe('request numbers and subjects', () => {
  it('reads r12 and 12', () => {
    expect(parseRequestNum('r12')).toBe(12);
    expect(parseRequestNum(' R3 ')).toBe(3);
    expect(parseRequestNum('7')).toBe(7);
    expect(parseRequestNum('r0')).toBeNull();
    expect(parseRequestNum('t3')).toBeNull();
    expect(parseRequestNum(undefined)).toBeNull();
  });
  it('finds [r12] tags in subjects, once each', () => {
    expect(subjectRequestTags('Re: [r4] ship [t2] [r5] [R4]')).toEqual([4, 5]);
    expect(subjectRequestTags('r4 without brackets')).toEqual([]);
    expect(subjectRequestTags(undefined)).toEqual([]);
  });
  it('knows the creation mail of a request', () => {
    const r = req(4, { thread: 40 });
    expect(isCreationMail({ subject: '[r4] Question 4', thread_id: 40 }, r)).toBe(true);
    expect(isCreationMail({ subject: 'Re: [r4] Question 4', thread_id: 40 }, r)).toBe(false);
    expect(isCreationMail({ subject: '[r4] Question 4', thread_id: 41 }, r)).toBe(false);
    expect(isCreationMail({ subject: '[r40] other', thread_id: 40 }, r)).toBe(false);
  });
});

describe('the queue', () => {
  it('groups by class, now first, oldest first in each', () => {
    const g = groupByClass([req(3, { class: 'later' }), req(2), req(1), req(5, { class: 'now' })]);
    expect(g.map((x) => x.cls)).toEqual(['now', 'soon', 'later']);
    expect(g[1].requests.map((r) => r.num)).toEqual([1, 2]);
    expect(groupByClass([req(1)]).map((x) => x.cls)).toEqual(['soon']);
  });
  it('offers the defaults only when every request has one', () => {
    expect(allHaveDefault([req(1, { default: 'yes' }), req(2, { default: 'no' })])).toBe(true);
    expect(allHaveDefault([req(1, { default: 'yes' }), req(2)])).toBe(false);
    expect(allHaveDefault([req(1, { default: 'gone' })])).toBe(false);
    expect(allHaveDefault([])).toBe(false);
  });
  it('words the answer', () => {
    expect(answerText(req(1, { state: 'answered', answer: { option: 'no', by: 'owner', at: '' } }))).toBe('Hold it');
    expect(answerText(req(1, { state: 'answered', answer: { text: 'Use trigrams', by: 'owner', at: '' } }))).toBe('Use trigrams');
    expect(answerText(req(1))).toBe('');
  });
  it('splits the requests from the other entries without duplicates', () => {
    const attn = req(7, { kind: 'attention', seat: 'fe-lead', attention: 'permission' });
    const q = req(8, { thread: 80 });
    const items: AttentionItem[] = [
      { kind: 'request', since: attn.created_at, seat: 'fe-lead', request: attn },
      { kind: 'request', since: q.created_at, seat: 'be-1', request: q },
      mail(80, '[r8] Question 8'),
      mail(81, 'Re: [r8] Question 8', 80),
      mail(90, 'Unrelated'),
      { kind: 'seat', since: '', seat: 'fe-lead', attention: 'permission' },
      { kind: 'seat', since: '', seat: 'be-2', attention: 'wake-failed' },
    ];
    const { requests, rest } = splitAttention(items);
    expect(requests.map((r) => r.num)).toEqual([7, 8]);
    expect(rest.map((i) => i.message?.id ?? i.seat)).toEqual([81, 90, 'be-2']);
  });
  it('keys request items by number', () => {
    const a: AttentionItem = { kind: 'request', since: '', seat: 'be-1', request: req(3) };
    const b: AttentionItem = { kind: 'request', since: '', seat: 'be-1', request: req(4) };
    expect(itemKey(a)).not.toBe(itemKey(b));
    expect(attentionKey(a)).toBe('req:3');
    const note = describeNote(a, (k, p) => `${k} ${JSON.stringify(p ?? {})}`);
    expect(note.title).toContain('notify.request');
    expect(note.title).toContain('r3');
    expect(note.body).toBe('Question 3');
  });
});

describe('snoozes', () => {
  it('hides a card for an hour and forgets what ended', () => {
    const now = 1_000_000;
    let s = addSnooze({}, 3, now);
    expect(isSnoozed(s, 3, now + SNOOZE_MS - 1)).toBe(true);
    expect(isSnoozed(s, 3, now + SNOOZE_MS)).toBe(false);
    expect(isSnoozed(s, 4, now)).toBe(false);
    s = addSnooze(s, 4, now + SNOOZE_MS + 5);
    expect(Object.keys(s)).toEqual(['4']);
  });
  it('reads what was stored, leniently', () => {
    expect(parseSnoozes(null, 0)).toEqual({});
    expect(parseSnoozes('not json', 0)).toEqual({});
    expect(parseSnoozes('{"3": 50, "4": 5, "x": 99, "5": "9"}', 10)).toEqual({ '3': 50 });
  });
});

describe('seats', () => {
  it('reads the request a seat waits for', () => {
    expect(waitingRequest(undefined)).toBeUndefined();
    expect(waitingRequest({})).toBeUndefined();
    expect(waitingRequest({ waiting: 12 })).toBe(12);
    expect(waitingRequest({ waiting: 'r12' })).toBe(12);
    expect(waitingRequest({ waiting: true })).toBeUndefined();
    expect(waitingRequest({ waiting_request: 5, waiting: 'r9' })).toBe(5);
  });
  it('offers an interrupt except for Claude Code', () => {
    expect(interruptKind('codex', undefined)).toBe('interrupt');
    expect(interruptKind('dsh', 'dsh')).toBe('interrupt');
    expect(interruptKind('claude', undefined)).toBe('terminal');
    expect(interruptKind('codex', 'claude')).toBe('terminal');
    expect(interruptKind(undefined, undefined)).toBe('interrupt');
  });
});

describe('the list filters', () => {
  it('reads the query and drops unknown values', () => {
    expect(parseFilters(new URLSearchParams(''))).toEqual({ state: 'all', kind: '', seat: '', cls: '' });
    expect(parseFilters(new URLSearchParams('state=open&kind=plan&seat=be-1&class=now'))).toEqual({ state: 'open', kind: 'plan', seat: 'be-1', cls: 'now' });
    expect(parseFilters(new URLSearchParams('state=bogus&kind=x&class=y'))).toEqual({ state: 'all', kind: '', seat: '', cls: '' });
  });
});
