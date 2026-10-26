// Tasks as the interface shows them: states, priorities, the order of a
// board column, the rank a dragged card gets, the filters of the table and
// the seat lists computed from listings.

import type { StoredTaskState, Task, TaskPriority, TaskRef, TaskState } from './types';

/** The board's columns, in order (as `GET /v1/tasks/board` answers them). */
export const TASK_STATES: TaskState[] = ['ready', 'open', 'doing', 'blocked', 'review', 'done', 'cancelled'];
/** The states a task can be set to. */
export const STORED_STATES: StoredTaskState[] = ['open', 'doing', 'blocked', 'review', 'done', 'cancelled'];
/** Urgent first. */
export const PRIORITIES: TaskPriority[] = ['urgent', 'high', 'normal', 'low'];

/** 0 for `urgent` up to 3 for `low` (the sort order). */
export function priorityOrder(p: TaskPriority | string): number {
  const i = PRIORITIES.indexOf(p as TaskPriority);
  return i < 0 ? 2 : i;
}

export function isClosed(s: TaskState | string): boolean {
  return s === 'done' || s === 'cancelled';
}

/** The states a task in stored state `from` can be set to: from a closed
 * task only `open` and `doing` (reopening); from any other state every
 * stored state. The current state is left out. */
export function nextStates(from: StoredTaskState): StoredTaskState[] {
  if (isClosed(from)) return ['open', 'doing'];
  return STORED_STATES.filter((s) => s !== from);
}

/** `t12`. */
export function taskId(num: number): string {
  return `t${num}`;
}

/** A task number from `t12`, `T12`, `#12` or `12`; null otherwise. */
export function parseTaskNum(s: string | undefined | null): number | null {
  const m = /^\s*[tT#]?(\d{1,15})\s*$/.exec(s ?? '');
  if (!m) return null;
  const n = Number(m[1]);
  return n > 0 ? n : null;
}

/** The order of a board column and of the personal lists' groups: by
 * priority (urgent first), then rank, then number. */
export function compareTasks(a: Pick<Task, 'priority' | 'rank' | 'num'>, b: Pick<Task, 'priority' | 'rank' | 'num'>): number {
  return priorityOrder(a.priority) - priorityOrder(b.priority) || a.rank - b.rank || a.num - b.num;
}

export function sortTasks<T extends Pick<Task, 'priority' | 'rank' | 'num'>>(tasks: T[]): T[] {
  return [...tasks].sort(compareTasks);
}

/** Rank spacing of new tasks on the server (`MAX(rank) + 1024`). */
export const RANK_STEP = 1024;

export interface RankWrite {
  num: number;
  rank: number;
}

/**
 * The rank writes that put card `moved` of a column at `index`, where
 * `index` is a position in the column without the moved card (0: first).
 * `column` is the column in its shown order (see `compareTasks`).
 *
 * Cards sort by priority before rank, so a card stays among the cards of
 * its own priority: a drop above them puts it first among them, a drop
 * below them last. The new rank is the midpoint of the neighbours' ranks
 * (one step before the first, one step after the last). When the
 * neighbours' ranks leave no integer between them, the cards of that
 * priority in the column are renumbered in steps of `RANK_STEP` from the
 * first one's rank, and every card whose rank changes is written. An empty
 * answer means the card does not move.
 */
export function rankForDrop(column: Pick<Task, 'num' | 'priority' | 'rank'>[], moved: number, index: number): RankWrite[] {
  const card = column.find((c) => c.num === moved);
  if (!card) return [];
  const others = column.filter((c) => c.num !== moved);
  const at = Math.max(0, Math.min(index, others.length));
  const group = others.filter((c) => c.priority === card.priority);
  // The position among the cards of the same priority.
  const pos = others.slice(0, at).filter((c) => c.priority === card.priority).length;
  const before = group[pos - 1];
  const after = group[pos];
  // Where the card is now among them: no change when it stays there.
  const ownGroup = column.filter((c) => c.priority === card.priority);
  if (ownGroup.findIndex((c) => c.num === moved) === pos) return [];
  if (!before && !after) return [];
  if (!before) return [{ num: moved, rank: after.rank - RANK_STEP }];
  if (!after) return [{ num: moved, rank: before.rank + RANK_STEP }];
  if (after.rank - before.rank >= 2) return [{ num: moved, rank: Math.floor((before.rank + after.rank) / 2) }];
  // No room between the neighbours: renumber the group with the card at its place.
  const order = [...group.slice(0, pos), card, ...group.slice(pos)];
  const start = Math.min(...order.map((c) => c.rank));
  const writes: RankWrite[] = [];
  order.forEach((c, i) => {
    const rank = start + i * RANK_STEP;
    if (c.rank !== rank || c.num === moved) writes.push({ num: c.num, rank });
  });
  // The moved card first: it is the change the person made.
  writes.sort((a, b) => (a.num === moved ? -1 : b.num === moved ? 1 : 0));
  return writes;
}

/** A state's colour class (the stylesheet's tokens). */
export function stateTone(s: TaskState | string): 'ok' | 'accent' | 'warn' | 'danger' | 'neutral' | 'notice' {
  switch (s) {
    case 'ready':
      return 'ok';
    case 'doing':
      return 'accent';
    case 'blocked':
      return 'danger';
    case 'review':
      return 'notice';
    case 'open':
      return 'warn';
    default:
      return 'neutral';
  }
}

/** The task is due before `now` and not finished. */
export function overdue(task: Pick<Task, 'due' | 'state'>, now: number): boolean {
  if (!task.due || isClosed(task.state)) return false;
  const d = Date.parse(task.due);
  return !Number.isNaN(d) && d < now;
}

/** The seat a waiting task belongs to: its assignee, else its reporter. */
export function ownerOf(task: Pick<Task, 'assignee' | 'reporter'>): string {
  return task.assignee ?? task.reporter;
}

const PERSONAL: TaskState[] = ['ready', 'doing', 'review', 'blocked', 'open'];

/** The order of `GET /v1/tasks/mine`: `ready` first, then `doing`,
 * `review`, `blocked`, `open`; within each by priority, rank and number. */
export function personalOrder<T extends Task>(tasks: T[]): T[] {
  const g = (s: TaskState) => {
    const i = PERSONAL.indexOf(s);
    return i < 0 ? PERSONAL.length : i;
  };
  return [...tasks].sort((a, b) => g(a.state) - g(b.state) || compareTasks(a, b));
}

/** One entry of a seat's "owed" list: a task another seat waits for. */
export interface OwedEntry {
  task: Task;
  /** The seat that waits. */
  seat?: string;
  /** The task that waits for `task`; absent for a task `blocked` on the seat. */
  blocks?: TaskRef;
}

/**
 * What `GET /v1/tasks/owed` answers for `seat`, computed from listings
 * (that call answers for the caller only, the user seat in the browser):
 * tasks `blocked` with `waiting_on` the seat, and the seat's unfinished
 * tasks (assigned to it, or unassigned and reported by it) that are the
 * last open blocker of another seat's task.
 *
 * `waiting` holds the org's unfinished tasks that are `blocked` or `open`
 * with blockers; `own` the seat's unfinished tasks.
 */
export function owedFor(seat: string, waiting: Task[], own: Task[]): OwedEntry[] {
  const mine = new Map(own.filter((t) => !isClosed(t.state) && (t.assignee === seat || (!t.assignee && t.reporter === seat))).map((t) => [t.num, t]));
  const out: OwedEntry[] = [];
  for (const w of waiting) {
    if (w.state === 'blocked' && w.waiting_on === seat) out.push({ task: w, seat: ownerOf(w) });
  }
  for (const w of waiting) {
    if (w.state !== 'open' || w.waits_on?.length !== 1) continue;
    const blocker = mine.get(w.waits_on[0]);
    if (!blocker || ownerOf(w) === seat) continue;
    out.push({ task: blocker, seat: ownerOf(w), blocks: { num: w.num, title: w.title, state: w.state, assignee: w.assignee } });
  }
  return out;
}

/** Filters of the table (`GET /v1/tasks`). Empty strings are left out. */
export interface TaskFilters {
  /** '' (every unfinished state), `all`, or one shown state. */
  state: string;
  assignee: string;
  reporter: string;
  team: string;
  priority: string;
  label: string;
  parent: string;
  /** A `datetime-local` value. */
  due_before: string;
  stale: boolean;
  text: string;
  /** '', `ok`, `rejected` or `none`. */
  result: string;
}

export const NO_FILTERS: TaskFilters = {
  state: '',
  assignee: '',
  reporter: '',
  team: '',
  priority: '',
  label: '',
  parent: '',
  due_before: '',
  stale: false,
  text: '',
  result: '',
};

/** Saved filters, with unknown or malformed fields left at their defaults. */
export function parseFilters(saved: string | null): TaskFilters {
  const f: TaskFilters = { ...NO_FILTERS };
  if (!saved) return f;
  try {
    const o = JSON.parse(saved) as Record<string, unknown>;
    for (const k of Object.keys(NO_FILTERS) as (keyof TaskFilters)[]) {
      const v = o[k];
      if (k === 'stale') f.stale = v === true;
      else if (typeof v === 'string') f[k] = v;
    }
  } catch {
    // Not JSON: the defaults.
  }
  return f;
}

/** The query parameters of `GET /v1/tasks` for the filters. */
export function filterParams(f: TaskFilters): Record<string, string | undefined> {
  const parent = f.parent.trim() ? (parseTaskNum(f.parent) ?? f.parent.trim()) : undefined;
  return {
    state: f.state || undefined,
    assignee: f.assignee || undefined,
    reporter: f.reporter || undefined,
    team: f.team || undefined,
    priority: f.priority || undefined,
    label: f.label.trim() || undefined,
    parent: parent === undefined ? undefined : String(parent),
    due_before: f.due_before ? localToIso(f.due_before) : undefined,
    stale: f.stale ? '1' : undefined,
    text: f.text.trim() || undefined,
    result: f.result || undefined,
  };
}

/** A `datetime-local` value (local time) as RFC 3339 in UTC; '' if invalid. */
export function localToIso(v: string): string {
  if (!v) return '';
  const d = new Date(v);
  return Number.isNaN(d.getTime()) ? '' : d.toISOString();
}

/** RFC 3339 as a `datetime-local` value in local time; '' if invalid. */
export function isoToLocal(iso: string | undefined): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** Labels from a comma-separated input: trimmed, empty ones and repeats left out. */
export function parseLabels(s: string): string[] {
  const out: string[] = [];
  for (const l of s.split(',')) {
    const v = l.trim();
    if (v && !out.includes(v)) out.push(v);
  }
  return out;
}

/** Error codes of the task calls that the interface has words for. */
export const TASK_ERRORS = [
  'invalid_transition',
  'not_blocked',
  'dependency_cycle',
  'dependency_too_deep',
  'too_many_blockers',
  'too_many_blocked',
  'too_many_links',
  'taken',
  'closed',
  'storage_quota',
  'not_found',
  'rate_limited',
  'steal_pending',
  'not_assigned',
  'own_task',
  'started',
  'not_assignee',
  'children_open',
  'not_done',
  'stale',
] as const;
export type TaskErrorCode = (typeof TASK_ERRORS)[number];

/** The code an error answer is shown with, or null for the server's message. */
export function taskErrorCode(status: number, code: string): TaskErrorCode | null {
  if ((TASK_ERRORS as readonly string[]).includes(code)) return code as TaskErrorCode;
  if (status === 404) return 'not_found';
  if (status === 429) return 'rate_limited';
  return null;
}

/** `t9, t11 (ok needed)`: blockers a task waits on, marking those whose
 * dependency needs an ok result with `okNeeded`. */
export function waitsList(task: Pick<Task, 'waits_on' | 'waits_on_ok'>, okNeeded: string): string {
  return (task.waits_on ?? []).map((n) => (task.waits_on_ok?.includes(n) ? `${taskId(n)} (${okNeeded})` : taskId(n))).join(', ');
}

/** The children of a task that are not closed. */
export function openChildren(children: TaskRef[]): TaskRef[] {
  return children.filter((c) => !isClosed(c.state));
}

/** The order of the report's sections. */
export const REPORT_SECTIONS = ['not_started', 'in_progress', 'review', 'rejected', 'approved', 'parents', 'blocked'] as const;

/** Whether a `task` event for `changed` (task numbers; `null`: everything
 * may have changed) concerns a view that shows the tasks `shown`. */
export function concerns(changed: Set<number> | null, shown: Iterable<number>): boolean {
  if (changed === null) return true;
  for (const n of shown) if (changed.has(n)) return true;
  return false;
}
