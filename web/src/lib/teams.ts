// The Teams view: one tree per top-level team (nested teams below their
// parent), then the seats outside any team.

import { buildTree, flatten } from './tree';
import type { Team } from './types';

export type TeamRow =
  | { type: 'team'; name: string; depth: number; team: Team }
  | { type: 'seat'; name: string; depth: number; team?: string; root: boolean; contact: boolean }
  | { type: 'loose'; depth: 0 };

const collator = new Intl.Collator('en', { numeric: true });

/**
 * Rows of the Teams view. Teams nest by `parent` (an unknown parent, or a
 * cycle, makes a team top-level). Inside a team, its root seat comes
 * first, then the members arranged by their `parent` seat within the
 * team. Seats in no team (or in a team that does not exist) follow under
 * one `loose` heading.
 */
export function teamRows(teams: Team[], seats: { name: string; parent?: string; kind?: 'agent' | 'user'; group?: string }[]): TeamRow[] {
  const byName = new Map(teams.map((t) => [t.name, t]));
  const parentOf = (t: Team): string | undefined => (t.parent && byName.has(t.parent) && t.parent !== t.name ? t.parent : undefined);
  // Break cycles.
  const top = new Set<string>();
  for (const t of teams) {
    const seen = new Set([t.name]);
    let p = parentOf(t);
    while (p !== undefined) {
      if (seen.has(p)) {
        top.add(t.name);
        break;
      }
      seen.add(p);
      p = parentOf(byName.get(p)!);
    }
  }
  const children = new Map<string, Team[]>();
  const roots: Team[] = [];
  for (const t of teams) {
    const p = top.has(t.name) ? undefined : parentOf(t);
    if (p === undefined) roots.push(t);
    else children.set(p, [...(children.get(p) ?? []), t]);
  }
  const order = (a: Team, b: Team) => collator.compare(a.name, b.name);
  roots.sort(order);
  for (const c of children.values()) c.sort(order);

  const seatMap = new Map(seats.map((s) => [s.name, s]));
  const placed = new Set<string>();
  const rows: TeamRow[] = [];
  const walk = (t: Team, depth: number) => {
    rows.push({ type: 'team', name: t.name, depth, team: t });
    const members = t.members.filter((m) => seatMap.has(m));
    const inTeam = new Set(members);
    const tree = buildTree(
      members.map((m) => {
        const s = seatMap.get(m)!;
        // The root seat heads the team; parents outside the team do not count.
        const parent = m === t.root ? undefined : s.parent && inTeam.has(s.parent) ? s.parent : t.root && inTeam.has(t.root) ? t.root : undefined;
        return { name: m, parent, kind: s.kind };
      }),
    );
    for (const r of flatten(tree, new Set())) {
      placed.add(r.name);
      rows.push({ type: 'seat', name: r.name, depth: depth + 1 + r.depth, team: t.name, root: r.name === t.root, contact: t.contacts.includes(r.name) });
    }
    for (const c of children.get(t.name) ?? []) walk(c, depth + 1);
  };
  for (const r of roots) walk(r, 0);

  const loose = seats.filter((s) => !placed.has(s.name));
  if (loose.length) {
    rows.push({ type: 'loose', depth: 0 });
    // A parent in a team does not count here (buildTree drops unknown parents).
    const tree = buildTree(loose);
    for (const r of flatten(tree, new Set())) rows.push({ type: 'seat', name: r.name, depth: 1 + r.depth, root: false, contact: false });
  }
  return rows;
}
