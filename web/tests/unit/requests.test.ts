import { describe, expect, it } from 'vitest';
import {
  SNOOZE_MS,
  addSnooze,
  allHaveDefault,
  countNeeds,
  answerText,
  groupByClass,
  interleave,
  interruptKind,
  isCreationMail,
  isSnoozed,
  parseFilters,
  parseRequestNum,
  parseSnoozes,
  requestMarks,
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
  it('knows the creation mail of a request by its message_id', () => {
    const r = req(4, { thread: 40, message_id: 40 });
    expect(isCreationMail({ id: 40 }, r)).toBe(true);
    expect(isCreationMail({ id: 41 }, r)).toBe(false);
    // Without message_id (an `attention` request) no mail is its creation mail.
    expect(isCreationMail({ id: 40 }, req(4, { thread: 40 }))).toBe(false);
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
    const q = req(8, { thread: 80, message_id: 80 });
    const items: AttentionItem[] = [
      { kind: 'request', since: attn.created_at, seat: 'fe-lead', request: attn, hint: 'answer it in the pane' },
      { kind: 'request', since: q.created_at, seat: 'be-1', request: q },
      mail(80, '[r8] Question 8'),
      mail(81, 'Re: [r8] Question 8', 80),
      mail(90, 'Unrelated'),
      { kind: 'seat', since: '', seat: 'fe-lead', attention: 'permission' },
      { kind: 'seat', since: '', seat: 'be-2', attention: 'wake-failed' },
      { kind: 'future' as AttentionItem['kind'], since: '', seat: 'x' },
      { kind: 'request', since: '', seat: 'y' },
    ];
    const { requests, items: byNum, rest } = splitAttention(items);
    expect(requests.map((r) => r.num)).toEqual([7, 8]);
    // The item of a request keeps the seat's details.
    expect(byNum.get(7)?.hint).toBe('answer it in the pane');
    expect([...byNum.keys()]).toEqual([7, 8]);
    expect(rest.map((i) => i.message?.id ?? i.seat)).toEqual([81, 90, 'fe-lead', 'be-2']);
    // fe-lead counts once, with its attention request.
    expect(countNeeds(requests, rest)).toBe(2 + 3);
    expect(countNeeds([q], rest)).toBe(1 + 4);
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
    expect(waitingRequest({ waiting_for: 12 })).toBe(12);
    expect(waitingRequest({ waiting_for: 0 })).toBeUndefined();
    // The names guessed before the node's field existed are not read.
    expect(waitingRequest({ waiting: 12, waiting_request: 5 } as never)).toBeUndefined();
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

describe('request marks in the conversation mirror', () => {
  const asked = req(1, { created_at: '2026-10-29T10:00:00Z' });
  const answered = req(2, {
    created_at: '2026-10-29T10:01:00Z',
    state: 'answered',
    answered_at: '2026-10-29T10:05:00Z',
    answer: { option: 'no', by: 'owner', at: '2026-10-29T10:05:00Z' },
  });
  const expired = req(3, {
    created_at: '2026-10-29T10:02:00Z',
    state: 'expired',
    answered_at: '2026-10-29T10:06:00Z',
    answer: { option: 'yes', by: 'deadline', at: '2026-10-29T10:06:00Z' },
  });
  const withdrawn = req(4, { created_at: '2026-10-29T10:03:00Z', state: 'withdrawn' });

  it('marks creation and closing, oldest first', () => {
    const m = requestMarks([withdrawn, expired, answered, asked], new Map([[4, { at: '2026-10-29T10:04:00Z', by: 'owner' }]]));
    expect(m.map((x) => [x.num, x.what])).toEqual([
      [1, 'created'],
      [2, 'created'],
      [3, 'created'],
      [4, 'created'],
      [4, 'withdrawn'],
      [2, 'answered'],
      [3, 'expired'],
    ]);
    expect(m.find((x) => x.what === 'answered')).toMatchObject({ answer: 'Hold it', by: 'owner' });
    // A withdrawal without a known time is left out.
    expect(requestMarks([withdrawn]).map((x) => x.what)).toEqual(['created']);
  });

  it('puts the marks between the events by time', () => {
    const ev = (ts: string | undefined, text: string) => ({ ts, text });
    const events = [ev('2026-10-29T10:00:30Z', 'a'), ev(undefined, 'b'), ev('2026-10-29T10:04:30Z', 'c')];
    const marks = requestMarks([asked, answered]);
    const out = interleave(events, marks).map((x) => ('mark' in x ? `r${x.mark.num}:${x.mark.what}` : x.ev.text));
    // r1 was made before the first event shown: left out.
    expect(out).toEqual(['a', 'b', 'r2:created', 'c', 'r2:answered']);
    expect(interleave(events, []).map((x) => ('ev' in x ? x.i : -1))).toEqual([0, 1, 2]);
    // Without times there is nothing to place the marks by.
    expect(interleave([ev(undefined, 'x')], marks)).toEqual([{ ev: ev(undefined, 'x'), i: 0 }]);
  });
});
