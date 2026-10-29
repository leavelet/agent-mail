// The check-box selection of a message list, and the plans of the bulk
// read actions. Pure functions over ids; the page keeps the state.

import type { InboxEntry } from './types';

/** "Mark all read" asks first when it would mark more than this many. */
export const CONFIRM_ABOVE = 20;

/** A new selection with `id` added (`on`) or removed. */
export function toggled(sel: ReadonlySet<number>, id: number, on = !sel.has(id)): Set<number> {
  const next = new Set(sel);
  if (on) next.add(id);
  else next.delete(id);
  return next;
}

/** The selection's ids that are still rows of the list, in list order. */
export function selectedRows(sel: ReadonlySet<number>, ids: readonly number[]): number[] {
  return ids.filter((id) => sel.has(id));
}

/** The state of the header check box over the list's rows. */
export function headerState(sel: ReadonlySet<number>, ids: readonly number[]): 'none' | 'some' | 'all' {
  const n = selectedRows(sel, ids).length;
  if (n === 0) return 'none';
  return n === ids.length ? 'all' : 'some';
}

/** The header check box: select every row, or none when all are selected. */
export function toggleAll(sel: ReadonlySet<number>, ids: readonly number[]): Set<number> {
  return headerState(sel, ids) === 'all' ? new Set() : new Set(ids);
}

/** Ids among `ids` whose delivery can be marked read (unread, not withdrawn). */
export function readable(entries: readonly InboxEntry[], ids: readonly number[]): number[] {
  const want = new Set(ids);
  return entries
    .filter((e) => want.has(e.message.id) && !e.delivery.read_at && !e.delivery.withdrawn_at)
    .map((e) => e.message.id);
}

/** Ids among `ids` whose delivery can be marked unread (read, not acknowledged). */
export function unreadable(entries: readonly InboxEntry[], ids: readonly number[]): number[] {
  const want = new Set(ids);
  return entries
    .filter((e) => want.has(e.message.id) && !!e.delivery.read_at && !e.delivery.acked_at)
    .map((e) => e.message.id);
}

/** The `before_seq` of "Mark all read" over the loaded entries: mail that
 * arrived after the newest loaded entry stays unread. `undefined` when
 * nothing is loaded. */
export function markAllBefore(entries: readonly InboxEntry[]): number | undefined {
  if (!entries.length) return undefined;
  return Math.max(...entries.map((e) => e.delivery.seq)) + 1;
}

/** The entries with the deliveries of `ids` marked read (`read`) or not
 * read, as the server does it, so that the rows change before the list is
 * fetched again. */
export function withRead(entries: readonly InboxEntry[], ids: readonly number[], read: boolean, now: string): InboxEntry[] {
  const want = new Set(ids);
  return entries.map((e) => {
    if (!want.has(e.message.id)) return e;
    const d = e.delivery;
    if (read) {
      if (d.read_at || d.withdrawn_at) return e;
      return { ...e, delivery: { ...d, read_at: now, notified_at: d.notified_at ?? now } };
    }
    if (!d.read_at || d.acked_at) return e;
    return { ...e, delivery: { ...d, read_at: undefined } };
  });
}
