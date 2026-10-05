import { describe, expect, it } from 'vitest';
import { activeOrgs, addedContacts, audiences, changeLines, contactsFor, grantsOf, orgOf, otherOrgs, peerCandidates, reaches, sendErrorKey } from '../../src/lib/openings';
import { teamRows } from '../../src/lib/teams';
import { translate } from '../../src/lib/i18n';
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
  it('says before sending which contact the server adds', () => {
    const addrs = ['help@beta', 'docs', 'x@gamma', 'y@beta'];
    expect(otherOrgs(addrs)).toEqual(['beta', 'gamma']);
    expect(addedContacts([{ address: 'help@beta' }, { address: 'lead', added: true }], addrs)).toEqual([{ seat: 'lead', owner: undefined, orgs: ['beta', 'gamma'] }]);
    // A contact of the other org already in the thread, added for this org.
    expect(addedContacts([{ address: 'liaison@beta', added: true, added_for: 'acme' }], addrs)).toEqual([{ seat: 'liaison', owner: 'beta', orgs: ['acme'] }]);
    expect(addedContacts([{ address: 'help@beta' }], addrs)).toEqual([]);
    expect(translate('en', 'compose.added', { seat: 'lead', org: 'beta' })).toBe("Your org's contact lead will be added in Cc: every message to beta carries a contact of each org.");
    expect(sendErrorKey('list_on_cross_org')).toBe('compose.err.listCrossOrg');
    expect(sendErrorKey('no_contact')).toBe('compose.err.noContact');
    expect(sendErrorKey('other')).toBeNull();
  });
  it('finds the contacts for an org', () => {
    const seats = [{ name: 'a', open_to: ['linked'] }, { name: 'b', open_to: ['beta'] }, { name: 'c', open_to: ['gamma'] }, { name: 'd' }];
    expect(contactsFor(seats, 'beta')).toEqual(['a', 'b']);
    expect(contactsFor(seats, 'delta')).toEqual(['a']);
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
