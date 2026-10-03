import { describe, expect, it } from 'vitest';
import { aggregate, ancestors, buildTree, flatten, parents } from '../../src/lib/tree';

const seats = [
  { name: 'owner', kind: 'user' as const },
  { name: 'root' },
  { name: 'be-lead', parent: 'root' },
  { name: 'be-10', parent: 'be-lead' },
  { name: 'be-2', parent: 'be-lead' },
  { name: 'fe-lead', parent: 'root' },
  { name: 'fe-1', parent: 'fe-lead' },
  { name: 'orphan', parent: 'gone' },
];

describe('tree', () => {
  it('builds roots and children with the user first and numeric order', () => {
    const t = buildTree(seats);
    expect(t.roots).toEqual(['owner', 'orphan', 'root']);
    expect(t.children.get('be-lead')).toEqual(['be-2', 'be-10']);
    expect(t.size.get('root')).toBe(5);
    expect(t.size.get('fe-1')).toBe(0);
  });

  it('flattens with collapse', () => {
    const t = buildTree(seats);
    const all = flatten(t, new Set());
    expect(all.map((r) => r.name)).toEqual(['owner', 'orphan', 'root', 'be-lead', 'be-2', 'be-10', 'fe-lead', 'fe-1']);
    expect(all.find((r) => r.name === 'be-2')!.depth).toBe(2);
    const some = flatten(t, new Set(['be-lead']));
    expect(some.map((r) => r.name)).toEqual(['owner', 'orphan', 'root', 'be-lead', 'fe-lead', 'fe-1']);
    expect(some.find((r) => r.name === 'be-lead')).toMatchObject({ hasChildren: true, expanded: false });
    const none = flatten(t, new Set(parents(t)));
    expect(none.map((r) => r.name)).toEqual(['owner', 'orphan', 'root']);
  });

  it('filters to matches and their ancestors, expanding through collapsed seats', () => {
    const t = buildTree(seats);
    const rows = flatten(t, new Set(['root', 'be-lead']), (n) => n === 'be-10');
    expect(rows.map((r) => [r.name, r.context])).toEqual([
      ['root', true],
      ['be-lead', true],
      ['be-10', false],
    ]);
    expect(flatten(t, new Set(), () => false)).toEqual([]);
  });

  it('breaks parent cycles', () => {
    const t = buildTree([
      { name: 'a', parent: 'b' },
      { name: 'b', parent: 'a' },
      { name: 'c', parent: 'c' },
    ]);
    expect(flatten(t, new Set()).length).toBe(3);
    expect(t.roots).toContain('c');
  });

  it('aggregates descendants and lists ancestors', () => {
    const t = buildTree(seats);
    const busy = new Set(['be-2', 'be-10', 'fe-1']);
    const agg = aggregate(t, (n) => ({ busy: busy.has(n) ? 1 : 0, needs: n === 'be-10' ? 1 : 0 }));
    expect(agg.get('root')).toEqual({ busy: 3, needs: 1 });
    expect(agg.get('be-lead')).toEqual({ busy: 2, needs: 1 });
    expect(agg.get('fe-1')).toEqual({});
    expect(ancestors(t, 'be-10')).toEqual(['be-lead', 'root']);
  });

  it('handles 1,000 seats quickly', () => {
    const many = [{ name: 'root' }];
    for (let g = 0; g < 50; g++) {
      many.push({ name: `g${g}-lead`, parent: 'root' } as never);
      for (let w = 0; w < 19; w++) many.push({ name: `g${g}-w${w}`, parent: `g${g}-lead` } as never);
    }
    const t0 = performance.now();
    const t = buildTree(many);
    const rows = flatten(t, new Set());
    const collapsed = flatten(t, new Set(parents(t).filter((p) => p !== 'root')));
    expect(rows.length).toBe(1001);
    expect(collapsed.length).toBe(51);
    expect(performance.now() - t0).toBeLessThan(200);
  });
});
