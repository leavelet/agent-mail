// The org tree: seats arranged by `parent`, flattened for a virtualized list.

export interface TreeSeat {
  name: string;
  parent?: string;
  kind?: 'agent' | 'user';
}

export interface Tree {
  roots: string[];
  children: Map<string, string[]>;
  parent: Map<string, string | undefined>;
  /** Number of descendants (not counting the seat itself). */
  size: Map<string, number>;
}

const collator = new Intl.Collator('en', { numeric: true });

export function buildTree(seats: TreeSeat[]): Tree {
  const names = new Set(seats.map((s) => s.name));
  const kind = new Map(seats.map((s) => [s.name, s.kind]));
  const parent = new Map<string, string | undefined>();
  for (const s of seats) parent.set(s.name, s.parent && names.has(s.parent) && s.parent !== s.name ? s.parent : undefined);
  // Break cycles: a seat whose ancestor chain returns to it becomes a root.
  for (const s of seats) {
    const seen = new Set<string>([s.name]);
    let p = parent.get(s.name);
    while (p !== undefined) {
      if (seen.has(p)) {
        parent.set(s.name, undefined);
        break;
      }
      seen.add(p);
      p = parent.get(p);
    }
  }
  const children = new Map<string, string[]>();
  const roots: string[] = [];
  for (const s of seats) {
    const p = parent.get(s.name);
    if (p === undefined) roots.push(s.name);
    else {
      const c = children.get(p);
      if (c) c.push(s.name);
      else children.set(p, [s.name]);
    }
  }
  const order = (a: string, b: string) => {
    const ua = kind.get(a) === 'user' ? 0 : 1;
    const ub = kind.get(b) === 'user' ? 0 : 1;
    return ua - ub || collator.compare(a, b);
  };
  roots.sort(order);
  for (const c of children.values()) c.sort(order);
  const size = new Map<string, number>();
  const count = (n: string): number => {
    let k = 0;
    for (const c of children.get(n) ?? []) k += 1 + count(c);
    size.set(n, k);
    return k;
  };
  for (const r of roots) count(r);
  return { roots, children, parent, size };
}

export interface Row {
  name: string;
  depth: number;
  hasChildren: boolean;
  expanded: boolean;
  /** Shown only because a descendant matches the filter. */
  context: boolean;
}

/**
 * Visible rows in display order. Collapsed seats hide their subtree. With
 * `match`, only matching seats and their ancestors are shown, and ancestors
 * of matches are expanded regardless of `collapsed`.
 */
export function flatten(tree: Tree, collapsed: Set<string>, match?: (name: string) => boolean): Row[] {
  const rows: Row[] = [];
  let hit: Map<string, boolean> | undefined;
  if (match) {
    hit = new Map();
    const mark = (n: string): boolean => {
      let any = match(n);
      for (const c of tree.children.get(n) ?? []) if (mark(c)) any = true;
      hit!.set(n, any);
      return any;
    };
    for (const r of tree.roots) mark(r);
  }
  const walk = (n: string, depth: number) => {
    if (hit && !hit.get(n)) return;
    const kids = tree.children.get(n) ?? [];
    const visibleKids = hit ? kids.filter((k) => hit!.get(k)) : kids;
    const expanded = hit ? visibleKids.length > 0 : !collapsed.has(n);
    rows.push({
      name: n,
      depth,
      hasChildren: kids.length > 0,
      expanded: kids.length > 0 && expanded,
      context: !!match && !match(n),
    });
    if (kids.length > 0 && expanded) for (const k of visibleKids) walk(k, depth + 1);
  };
  for (const r of tree.roots) walk(r, 0);
  return rows;
}

/** All seats that have children (for "collapse all"). */
export function parents(tree: Tree): string[] {
  return [...tree.children.keys()];
}

/** The chain of ancestors of `name`, nearest first. */
export function ancestors(tree: Tree, name: string): string[] {
  const out: string[] = [];
  let p = tree.parent.get(name);
  while (p !== undefined) {
    out.push(p);
    p = tree.parent.get(p);
  }
  return out;
}

/**
 * Per seat, the sum of `value` over its descendants (not itself), e.g. how
 * many seats below a collapsed lead are busy or need a person.
 */
export function aggregate(
  tree: Tree,
  value: (name: string) => Record<string, number>,
): Map<string, Record<string, number>> {
  const out = new Map<string, Record<string, number>>();
  const walk = (n: string): Record<string, number> => {
    const below: Record<string, number> = {};
    for (const c of tree.children.get(n) ?? []) {
      const sub = walk(c);
      const own = value(c);
      for (const src of [sub, own]) for (const [k, v] of Object.entries(src)) if (v) below[k] = (below[k] ?? 0) + v;
    }
    out.set(n, below);
    return below;
  };
  for (const r of tree.roots) walk(r);
  return out;
}
