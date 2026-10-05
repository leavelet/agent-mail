import { afterEach, describe, expect, it } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import { activeOrgs, audiences, changeLines, grantsOf, hiddenCount, maskedKey, orgOf, peerCandidates, reaches } from '../../src/lib/openings';
import { teamRows } from '../../src/lib/teams';
import { translate } from '../../src/lib/i18n';
import { ui } from '../../src/lib/ui.svelte';
import MessageCard from '../../src/components/MessageCard.svelte';
import type { Card, Grant, Team } from '../../src/lib/types';

const grant = (id: number, target: string, audience: string, active = true): Grant => ({ id, target, kind: 'agent', audience, active, created_at: 'x' });

describe('openings', () => {
  it('finds the grants of a target and whether they reach an org', () => {
    const gs = [grant(1, 'help', 'linked'), grant(2, 'help', 'beta', false), grant(3, 'ops', 'gamma')];
    expect(grantsOf(gs, 'help').map((g) => g.id)).toEqual([1, 2]);
    expect(reaches(gs[0], 'anyone')).toBe(true);
    expect(reaches(gs[2], 'beta')).toBe(false);
    expect(reaches(gs[2], 'gamma')).toBe(true);
  });
  it('turns a preview into lines, gains before losses', () => {
    expect(changeLines([])).toEqual([]);
    expect(
      changeLines([
        { org: 'beta', gains: ['help', 'ops (team)'] },
        { org: 'gamma', gains: ['x'], loses: ['y'] },
        { org: 'delta' },
      ]),
    ).toEqual([
      { org: 'beta', verb: 'gains', names: ['help', 'ops (team)'] },
      { org: 'gamma', verb: 'gains', names: ['x'] },
      { org: 'gamma', verb: 'loses', names: ['y'] },
    ]);
  });
  it('counts masked recipients', () => {
    expect(hiddenCount({})).toBe(0);
    expect(hiddenCount({ to_hidden: 2, cc_hidden: 1 })).toBe(3);
    expect(hiddenCount({ to_hidden: 2, cc_hidden: 1 }, 'cc')).toBe(1);
    expect(translate('en', maskedKey(3, true), { n: 3 })).toBe('and 3 recipients not shown');
    expect(translate('en', maskedKey(1, true), { n: 1 })).toBe('and 1 recipient not shown');
    expect(translate('en', maskedKey(2, false), { n: 2 })).toBe('2 recipients not shown');
    expect(translate('zh', 'mail.hidden.more', { n: 3 })).toBe('另有 3 位收件人未显示');
  });
  it('offers only open seats of active links and thread participants', () => {
    const open = new Map<string, Card[]>([
      ['beta', [
        { address: 'help@beta', kind: 'agent', description: 'support' },
        { address: 'eng@beta', kind: 'team' },
        { address: 'owner@beta', kind: 'user' },
      ]],
      // A stale entry for an org whose link ended.
      ['gone', [{ address: 'x@gone', kind: 'agent' }]],
      // A card that claims another org is ignored.
      ['gamma', [{ address: 'spoof@beta', kind: 'agent' }]],
    ]);
    const links = [
      { org: 'beta', state: 'active' as const },
      { org: 'gamma', state: 'active' as const },
      { org: 'gone', state: 'requested' as const },
    ];
    const parts: Card[] = [
      { address: 'help@beta', kind: 'agent' },
      { address: 'quiet@gamma', kind: 'agent', description: 'wrote in the thread' },
      { address: 'local', kind: 'agent' },
    ];
    expect(activeOrgs(links)).toEqual(['beta', 'gamma']);
    expect(peerCandidates(open, activeOrgs(links))).toEqual([
      { address: 'help@beta', kind: 'open', detail: 'support' },
      { address: 'owner@beta', kind: 'open', detail: undefined },
    ]);
    expect(peerCandidates(open, activeOrgs(links), parts).map((c) => `${c.address}:${c.kind}`)).toEqual([
      'help@beta:open',
      'owner@beta:open',
      'quiet@gamma:participant',
    ]);
    expect(orgOf('a@b')).toBe('b');
    expect(orgOf('a')).toBeUndefined();
    expect(audiences([{ org: 'zeta', state: 'invited' }, { org: 'beta', state: 'active' }])).toEqual(['linked', 'beta', 'zeta']);
  });
});

describe('teamRows', () => {
  const team = (name: string, o: Partial<Team> = {}): Team => ({ name, members: [], contacts: [], created_at: 'x', ...o });
  it('builds one tree per top-level team, nested teams, then seats outside any team', () => {
    const seats = [
      { name: 'owner', kind: 'user' as const },
      { name: 'cto', kind: 'agent' as const },
      { name: 'be-lead', parent: 'cto', kind: 'agent' as const },
      { name: 'be-1', parent: 'be-lead', kind: 'agent' as const },
      { name: 'db-1', parent: 'be-lead', kind: 'agent' as const },
      { name: 'fe-lead', parent: 'cto', kind: 'agent' as const },
      { name: 'help', kind: 'agent' as const },
    ];
    const teams = [
      team('frontend', { members: ['fe-lead'], root: 'fe-lead' }),
      team('backend', { members: ['be-lead', 'be-1'], root: 'be-lead', contacts: ['be-lead'], open_to: ['linked'] }),
      team('db', { parent: 'backend', members: ['db-1'] }),
    ];
    const rows = teamRows(teams, seats).map((r) => (r.type === 'loose' ? '--' : `${'.'.repeat(r.depth)}${r.type === 'team' ? '#' : ''}${r.name}${r.type === 'seat' && r.contact ? '*' : ''}`));
    expect(rows).toEqual(['#backend', '.be-lead*', '..be-1', '.#db', '..db-1', '#frontend', '.fe-lead', '--', '.owner', '.cto', '.help']);
  });
  it('survives cycles and unknown parents', () => {
    const rows = teamRows([team('a', { parent: 'b' }), team('b', { parent: 'a' }), team('c', { parent: 'nope' })], []);
    expect(rows.filter((r) => r.type === 'team').map((r) => r.type === 'team' && r.name).sort()).toEqual(['a', 'b', 'c']);
  });
});

describe('MessageCard masking', () => {
  let m: Record<string, unknown> | null = null;
  afterEach(() => {
    if (m) unmount(m);
    m = null;
    document.body.innerHTML = '';
  });
  it('shows how many recipients are not shown', () => {
    ui.lang = 'en';
    const target = document.createElement('div');
    document.body.appendChild(target);
    m = mount(MessageCard, {
      target,
      props: {
        message: {
          id: 5, thread_id: 5, from: 'help@beta', source: 'external', to: ['owner'], cc: [], to_hidden: 2, cc_hidden: 1,
          subject: 's', priority: 'normal', ack_required: false, lines: 1, created_at: '2026-10-04T12:00:00Z', body: 'b',
        },
      },
    }) as Record<string, unknown>;
    flushSync();
    expect(target.querySelector('[data-testid="to-hidden"]')?.textContent).toBe('and 2 recipients not shown');
    expect(target.querySelector('[data-testid="cc-hidden"]')?.textContent).toBe('1 recipient not shown');
  });
});
