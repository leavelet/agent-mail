import { describe, expect, it } from 'vitest';
import { buildSend, completeToken, currentToken, isAddress, parseAddresses, parseMeta, replySubject, suggest } from '../../src/lib/compose';

describe('compose', () => {
  it('parses address fields', () => {
    expect(parseAddresses(' be-lead, W1  ops@acme;w1 ')).toEqual(['be-lead', 'w1', 'ops@acme']);
    expect(isAddress('be-lead')).toBe(true);
    expect(isAddress('a@b')).toBe(true);
    expect(isAddress('Bad Name')).toBe(false);
    expect(isAddress('-x')).toBe(false);
  });
  it('validates meta', () => {
    expect(parseMeta([{ key: 'commit', value: 'abc' }, { key: '', value: '' }])).toEqual({ meta: { commit: 'abc' } });
    expect(parseMeta([{ key: 'Bad Key', value: 'x' }]).error).toEqual({ kind: 'metaKey', k: 'Bad Key' });
    expect(parseMeta([{ key: 'k', value: 'a\nb' }]).error).toEqual({ kind: 'metaValue', k: 'k' });
    expect(parseMeta([{ key: 'k', value: '1' }, { key: 'k', value: '2' }]).error).toEqual({ kind: 'metaDup', k: 'k' });
  });
  it('builds a send request', () => {
    const { req } = buildSend({
      to: 'w1, w2',
      cc: 'leads',
      subject: ' Hi ',
      body: 'text',
      urgent: true,
      ack: true,
      meta: [{ key: 'branch', value: 'amail/w1' }],
      inReplyTo: 7,
    });
    expect(req).toEqual({
      to: ['w1', 'w2'],
      cc: ['leads'],
      subject: 'Hi',
      body: 'text',
      in_reply_to: 7,
      priority: 'urgent',
      ack_required: true,
      meta: { branch: 'amail/w1' },
    });
    expect(buildSend({ to: '', cc: '', subject: '', body: '', urgent: false, ack: false, meta: [] }).error).toEqual({ kind: 'noTo' });
    expect(buildSend({ to: 'a b!', cc: '', subject: '', body: '', urgent: false, ack: false, meta: [] }).error).toEqual({ kind: 'addr', a: 'b!' });
  });
  it('replies and completes', () => {
    expect(replySubject('Plan')).toBe('Re: Plan');
    expect(replySubject('RE: Plan')).toBe('RE: Plan');
    expect(currentToken('w1, be-')).toBe('be-');
    expect(completeToken('w1, be-', 'be-lead')).toBe('w1, be-lead, ');
  });
  it('suggests seats, lists and linked-org addresses', () => {
    const seats = [{ name: 'be-lead' }, { name: 'be-1' }, { name: 'web' }];
    expect(suggest('be', seats, ['beta-team', 'ops'], ['acme']).map((s) => s.address)).toEqual(['be-1', 'be-lead', 'beta-team', 'be@acme']);
    expect(suggest('ops@a', seats, [], ['acme', 'beta']).map((s) => s.address)).toEqual(['ops@acme']);
  });
});
