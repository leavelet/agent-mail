import { describe, expect, it } from 'vitest';
import { MAX_SUBJECT_CHARS, SUBJECT_SHOWN_CHARS, shortSubject, subjectLength } from '../../src/lib/subject';
import { buildSend, replySubject } from '../../src/lib/compose';
import { describe as describeItem } from '../../src/lib/notify';

describe('subjects', () => {
  it('counts code points, not UTF-16 units', () => {
    expect(subjectLength('🚀🚀')).toBe(2);
    expect(subjectLength('é界')).toBe(2);
  });
  it('cuts one-line subjects to 200 characters and an ellipsis', () => {
    expect(shortSubject('Plan')).toBe('Plan');
    const exact = '界'.repeat(SUBJECT_SHOWN_CHARS);
    expect(shortSubject(exact)).toBe(exact);
    expect(shortSubject('界'.repeat(MAX_SUBJECT_CHARS))).toBe(`${'界'.repeat(200)}…`);
    // A character outside the BMP is never split into a lone surrogate.
    expect(shortSubject('🚀'.repeat(201))).toBe(`${'🚀'.repeat(200)}…`);
  });
  it('accepts subjects up to the server limit and keeps replies within it', () => {
    const base = { to: 'a', cc: '', body: '', urgent: false, ack: false, meta: [] };
    expect(buildSend({ ...base, subject: '🚀'.repeat(MAX_SUBJECT_CHARS) }).req?.subject).toBe('🚀'.repeat(MAX_SUBJECT_CHARS));
    expect(buildSend({ ...base, subject: '🚀'.repeat(MAX_SUBJECT_CHARS + 1) }).error).toEqual({ kind: 'subjectLength' });
    const r = replySubject('界'.repeat(MAX_SUBJECT_CHARS));
    expect(subjectLength(r)).toBe(MAX_SUBJECT_CHARS);
    expect(r.startsWith('Re: 界')).toBe(true);
  });
  it('notifications show the short subject', () => {
    const item = { kind: 'reply', message: { from: 'a', subject: '界'.repeat(300) } } as unknown as Parameters<typeof describeItem>[0];
    expect(describeItem(item, (k) => k).body).toBe(`${'界'.repeat(200)}…`);
  });
});

describe('written by', () => {
  it('notifications name the user who wrote a message in an agent\'s name', () => {
    const item = { kind: 'reply', message: { from: 'be-lead', written_by: 'owner', subject: 's' } } as unknown as Parameters<typeof describeItem>[0];
    const t = (k: string, p?: Record<string, string>) => (k === 'mail.writtenBy' ? `written by ${p?.user}` : `from ${p?.from}`);
    expect(describeItem(item, t).title).toBe('from be-lead (written by owner)');
  });
});

import { canWithdraw, deliveryState, deliveryStep, deliverySummary } from '../../src/lib/delivery';
import { fromEntry } from '../../src/lib/mailrows';

describe('withdrawn deliveries', () => {
  it('have a state of their own and can be withdrawn only while queued', () => {
    expect(deliveryState({ withdrawn_at: 'w' })).toBe('withdrawn');
    expect(deliveryState({ withdrawn_at: 'w', read_at: 'r' })).toBe('withdrawn');
    expect(deliveryStep('withdrawn')).toBe(-1);
    expect(deliverySummary({ queued: 1, notified: 0, read: 0, acked: 0, withdrawn: 2 })).toEqual([
      { state: 'queued', n: 1 },
      { state: 'withdrawn', n: 2 },
    ]);
    expect(canWithdraw('b', {})).toBe(true);
    expect(canWithdraw('b', { notified_at: 'n' })).toBe(false);
    expect(canWithdraw('b', { withdrawn_at: 'w' })).toBe(false);
    expect(canWithdraw('x@beta', {})).toBe(false);
  });
  it('are not unread in a mailbox', () => {
    const e = {
      delivery: { seat: 'owner', seq: 1, mod_seq: 2, class: 'to', mode: 'archive', withdrawn_at: 'w' },
      message: { id: 1, thread_id: 1, from: 'a', source: 'agent', to: ['owner'], cc: [], subject: 's', priority: 'normal', ack_required: false, lines: 1, created_at: 't' },
    } as unknown as Parameters<typeof fromEntry>[0];
    const row = fromEntry(e);
    expect(row.unread).toBe(false);
    expect(row.withdrawn).toBe(true);
  });
});
