import { describe, expect, it } from 'vitest';
import {
  RANK_STEP,
  compareTasks,
  concerns,
  filterParams,
  isClosed,
  isoToLocal,
  localToIso,
  NO_FILTERS,
  nextStates,
  overdue,
  owedFor,
  parseFilters,
  parseLabels,
  parseTaskNum,
  personalOrder,
  priorityOrder,
  rankForDrop,
  sortTasks,
  stateTone,
  taskErrorCode,
} from '../../src/lib/tasks';
import type { Task, TaskPriority, TaskState } from '../../src/lib/types';

function task(num: number, o: Partial<Task> = {}): Task {
  return {
    num,
    title: `task ${num}`,
    state: 'ready',
    stored_state: 'open',
    priority: 'normal',
    rank: num * RANK_STEP,
    reporter: 'owner',
    created_at: '2026-10-26T00:00:00Z',
    updated_at: '2026-10-26T00:00:00Z',
    ...o,
  };
}

const card = (num: number, rank: number, priority: TaskPriority = 'normal') => ({ num, rank, priority });

describe('priority and state helpers', () => {
  it('orders priorities urgent first', () => {
    expect(['low', 'urgent', 'normal', 'high'].map(priorityOrder)).toEqual([3, 0, 2, 1]);
  });
  it('sorts by priority, then rank, then number', () => {
    const list = [card(1, 10), card(2, 5, 'high'), card(3, 10), card(4, 1, 'low'), card(5, 2)];
    expect(sortTasks(list).map((c) => c.num)).toEqual([2, 5, 1, 3, 4]);
    expect(compareTasks(card(1, 10), card(1, 10))).toBe(0);
  });
  it('knows closed states and the allowed changes', () => {
    expect(isClosed('done') && isClosed('cancelled')).toBe(true);
    expect(isClosed('ready') || isClosed('blocked')).toBe(false);
    expect(nextStates('done')).toEqual(['open', 'doing']);
    expect(nextStates('cancelled')).toEqual(['open', 'doing']);
    expect(nextStates('open')).toEqual(['doing', 'blocked', 'review', 'done', 'cancelled']);
    expect(nextStates('doing')).not.toContain('doing');
    expect(nextStates('doing')).not.toContain('ready');
  });
  it('gives each state a tone', () => {
    const states: TaskState[] = ['ready', 'open', 'doing', 'blocked', 'review', 'done', 'cancelled'];
    expect(states.map(stateTone)).toEqual(['ok', 'warn', 'accent', 'danger', 'notice', 'neutral', 'neutral']);
  });
  it('parses task numbers', () => {
    expect(parseTaskNum('t12')).toBe(12);
    expect(parseTaskNum(' T9 ')).toBe(9);
    expect(parseTaskNum('#3')).toBe(3);
    expect(parseTaskNum('7')).toBe(7);
    expect(parseTaskNum('t0')).toBeNull();
    expect(parseTaskNum('x1')).toBeNull();
    expect(parseTaskNum('')).toBeNull();
  });
  it('finds overdue unfinished tasks', () => {
    const now = Date.parse('2026-10-26T12:00:00Z');
    expect(overdue({ due: '2026-10-26T11:00:00Z', state: 'doing' }, now)).toBe(true);
    expect(overdue({ due: '2026-10-26T13:00:00Z', state: 'doing' }, now)).toBe(false);
    expect(overdue({ due: '2026-10-26T11:00:00Z', state: 'done' }, now)).toBe(false);
    expect(overdue({ state: 'doing' }, now)).toBe(false);
  });
  it('maps error answers to their texts', () => {
    expect(taskErrorCode(422, 'dependency_cycle')).toBe('dependency_cycle');
    expect(taskErrorCode(409, 'taken')).toBe('taken');
    expect(taskErrorCode(404, 'not_found')).toBe('not_found');
    expect(taskErrorCode(404, 'http')).toBe('not_found');
    expect(taskErrorCode(400, 'bad_request')).toBeNull();
  });
});

describe('rankForDrop', () => {
  const col = [card(1, 1024), card(2, 2048), card(3, 3072), card(4, 4096)];

  it('takes the midpoint of the neighbours', () => {
    // t4 between t1 and t2: index 1 in the column without t4.
    expect(rankForDrop(col, 4, 1)).toEqual([{ num: 4, rank: 1536 }]);
    // t1 between t3 and t4.
    expect(rankForDrop(col, 1, 2)).toEqual([{ num: 1, rank: 3584 }]);
  });
  it('goes one step before the first and after the last', () => {
    expect(rankForDrop(col, 3, 0)).toEqual([{ num: 3, rank: 0 }]);
    expect(rankForDrop(col, 1, 3)).toEqual([{ num: 1, rank: 4096 + RANK_STEP }]);
    expect(rankForDrop(col, 1, 99)).toEqual([{ num: 1, rank: 4096 + RANK_STEP }]);
  });
  it('writes nothing when the card stays where it is', () => {
    expect(rankForDrop(col, 2, 1)).toEqual([]);
    expect(rankForDrop(col, 1, 0)).toEqual([]);
    expect(rankForDrop(col, 4, 3)).toEqual([]);
    expect(rankForDrop([card(1, 5)], 1, 0)).toEqual([]);
    expect(rankForDrop(col, 99, 0)).toEqual([]);
  });
  it('keeps a card among the cards of its priority', () => {
    const mixed = [card(1, 9000, 'urgent'), card(2, 100), card(3, 200), card(4, 50, 'low')];
    // t3 dropped above the urgent card: first among the normal ones.
    expect(rankForDrop(mixed, 3, 0)).toEqual([{ num: 3, rank: 100 - RANK_STEP }]);
    // t2 dropped below the low card: last among the normal ones.
    expect(rankForDrop(mixed, 2, 3)).toEqual([{ num: 2, rank: 200 + RANK_STEP }]);
    // The only card of its priority does not move.
    expect(rankForDrop(mixed, 1, 3)).toEqual([]);
  });
  it('renumbers the cards of the priority when the ranks leave no room', () => {
    const tight = [card(1, 10), card(2, 11), card(3, 11)];
    // t3 between t1 and t2 (10 and 11): renumbered from 10.
    const w = rankForDrop(tight, 3, 1);
    expect(w[0]).toEqual({ num: 3, rank: 10 + RANK_STEP });
    expect(w).toContainEqual({ num: 2, rank: 10 + 2 * RANK_STEP });
    expect(w.some((x) => x.num === 1)).toBe(false);
    // Applied, the column is in the new order.
    const after = tight.map((c) => ({ ...c, rank: w.find((x) => x.num === c.num)?.rank ?? c.rank }));
    expect(sortTasks(after).map((c) => c.num)).toEqual([1, 3, 2]);
  });
  it('puts the card where it was dropped for every position', () => {
    const base = [card(1, 0), card(2, 1024), card(3, 1025), card(4, 1026), card(5, 5000)];
    for (const moved of [1, 2, 3, 4, 5]) {
      for (let i = 0; i < base.length; i++) {
        const w = rankForDrop(base, moved, i);
        const after = base.map((c) => ({ ...c, rank: w.find((x) => x.num === c.num)?.rank ?? c.rank }));
        const order = sortTasks(after).map((c) => c.num);
        const expected = base.map((c) => c.num).filter((n) => n !== moved);
        expected.splice(i, 0, moved);
        expect(order, `t${moved} to ${i}`).toEqual(expected);
      }
    }
  });
});

describe('seat lists', () => {
  it('orders like mine: ready, doing, review, blocked, open', () => {
    const list = [
      task(1, { state: 'open', waits_on: [9] }),
      task(2, { state: 'blocked', stored_state: 'blocked' }),
      task(3, { state: 'doing', stored_state: 'doing' }),
      task(4, { state: 'review', stored_state: 'review' }),
      task(5, { state: 'ready' }),
      task(6, { state: 'ready', priority: 'urgent' }),
    ];
    expect(personalOrder(list).map((x) => x.num)).toEqual([6, 5, 3, 4, 2, 1]);
  });
  it('computes owed like the server', () => {
    const own = [task(1, { assignee: 'be-1', state: 'doing' }), task(2, { reporter: 'be-1' }), task(3, { assignee: 'fe-1' })];
    const waiting = [
      task(10, { state: 'blocked', stored_state: 'blocked', waiting_on: 'be-1', assignee: 'fe-2' }),
      task(11, { state: 'blocked', stored_state: 'blocked', waiting_on: 'fe-1', assignee: 'fe-2' }),
      task(12, { state: 'open', waits_on: [1], assignee: 'fe-1' }),
      task(13, { state: 'open', waits_on: [1, 3], assignee: 'fe-1' }),
      task(14, { state: 'open', waits_on: [2] }),
      task(15, { state: 'open', waits_on: [1], assignee: 'be-1' }),
    ];
    const o = owedFor('be-1', waiting, own);
    expect(o.map((x) => [x.task.num, x.seat, x.blocks?.num])).toEqual([
      [10, 'fe-2', undefined],
      [1, 'fe-1', 12],
      [2, 'owner', 14],
    ]);
  });
});

describe('filters', () => {
  it('reads saved filters and ignores what it does not know', () => {
    expect(parseFilters(null)).toEqual(NO_FILTERS);
    expect(parseFilters('not json')).toEqual(NO_FILTERS);
    const f = parseFilters(JSON.stringify({ state: 'blocked', stale: true, label: 3, extra: 'x' }));
    expect(f.state).toBe('blocked');
    expect(f.stale).toBe(true);
    expect(f.label).toBe('');
  });
  it('builds the query of the list', () => {
    const p = filterParams({ ...NO_FILTERS, assignee: 'none', parent: 't12', label: ' ui ', stale: true, text: '  ' });
    expect(p).toMatchObject({ assignee: 'none', parent: '12', label: 'ui', stale: '1', text: undefined, state: undefined });
    const d = filterParams({ ...NO_FILTERS, due_before: '2026-10-26T09:30' });
    expect(d.due_before).toBe(new Date('2026-10-26T09:30').toISOString());
  });
  it('converts local times and labels', () => {
    expect(localToIso('')).toBe('');
    expect(localToIso('nonsense')).toBe('');
    expect(isoToLocal(localToIso('2026-10-26T09:30'))).toBe('2026-10-26T09:30');
    expect(parseLabels(' a, b,,a , c ')).toEqual(['a', 'b', 'c']);
  });
  it('says which views a task event concerns', () => {
    expect(concerns(null, [])).toBe(true);
    expect(concerns(new Set([3]), [1, 3])).toBe(true);
    expect(concerns(new Set([4]), [1, 3])).toBe(false);
  });
});
