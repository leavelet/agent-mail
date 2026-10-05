// Pure helpers for openings (grants), other orgs, masked recipients and
// address completion across orgs.

import type { Card, Grant, Link, NewGrant, SightChange } from './types';

/** A change of openings waiting for the user to confirm it. */
export type Pending = { add: NewGrant; remove?: undefined } | { remove: Grant; add?: undefined };

/** The grants on one seat or team. */
export function grantsOf(grants: Grant[], target: string): Grant[] {
  return grants.filter((g) => g.target === target);
}

/** The grant's audience includes `org` (it is `linked` or that org). */
export function reaches(g: Grant, org: string): boolean {
  return g.audience === 'linked' || g.audience === org;
}

/** One line of a preview: which org gains or loses which cards. */
export interface ChangeLine {
  org: string;
  verb: 'gains' | 'loses';
  names: string[];
}

/** The lines of a preview, gains before losses per org, in the server's
 * order of orgs. Empty: no other org sees anything different. */
export function changeLines(changes: SightChange[]): ChangeLine[] {
  const out: ChangeLine[] = [];
  for (const c of changes) {
    if (c.gains?.length) out.push({ org: c.org, verb: 'gains', names: c.gains });
    if (c.loses?.length) out.push({ org: c.org, verb: 'loses', names: c.loses });
  }
  return out;
}

/** The number of addresses left out of To and Cc (null when none). */
export function hiddenCount(m: { to_hidden?: number; cc_hidden?: number }, which: 'to' | 'cc' | 'all' = 'all'): number {
  const to = m.to_hidden ?? 0;
  const cc = m.cc_hidden ?? 0;
  return which === 'to' ? to : which === 'cc' ? cc : to + cc;
}

/** The dictionary key of the masked-recipients text: `more` when some
 * addresses of the same field are shown ("and 3 recipients not shown"). */
export function maskedKey(n: number, more: boolean): 'mail.hidden' | 'mail.hidden.more' | 'mail.hidden1' | 'mail.hidden1.more' {
  if (n === 1) return more ? 'mail.hidden1.more' : 'mail.hidden1';
  return more ? 'mail.hidden.more' : 'mail.hidden';
}

/** Orgs with an active link. */
export function activeOrgs(links: Link[]): string[] {
  return links.filter((l) => l.state === 'active').map((l) => l.org);
}

/** An address of another org that can be offered while composing. */
export interface PeerCandidate {
  address: string;
  kind: 'open' | 'participant';
  detail?: string;
}

/**
 * The addresses of other orgs that compose may offer: the seats (not
 * teams: they cannot be addressed) that orgs with an active link opened to
 * this org, and the participants of the thread being answered. Nothing
 * else of other orgs. Each address once; an open seat wins over a
 * participant.
 */
export function peerCandidates(open: Map<string, Card[]>, active: string[], participants: Card[] = []): PeerCandidate[] {
  const out: PeerCandidate[] = [];
  const seen = new Set<string>();
  const add = (c: Card, kind: PeerCandidate['kind']) => {
    if (c.kind === 'team' || !c.address.includes('@') || seen.has(c.address)) return;
    seen.add(c.address);
    out.push({ address: c.address, kind, detail: c.description });
  };
  for (const org of active) for (const c of open.get(org) ?? []) if (c.address.endsWith(`@${org}`)) add(c, 'open');
  for (const c of participants) add(c, 'participant');
  return out;
}

/** The org part of an address (`name@org`), or undefined for a local one. */
export function orgOf(address: string): string | undefined {
  const i = address.indexOf('@');
  return i < 0 ? undefined : address.slice(i + 1);
}

/** Audiences offered for a new grant: every linked org, then each org
 * with a link of any state (a grant to an org without an active link
 * waits for it). */
export function audiences(links: Link[]): string[] {
  return ['linked', ...links.map((l) => l.org).sort()];
}
