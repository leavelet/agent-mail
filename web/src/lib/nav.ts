// Which sidebar entry is highlighted.

import type { Route } from './router.svelte';

export type Folder = 'inbox' | 'sent';

/** The section a route belongs to; the Sent tab of the inbox is `sent`. */
export function sectionOf(route: Route): string {
  const s = route.parts[0] ?? 'org';
  return s === 'inbox' && route.query.get('tab') === 'sent' ? 'sent' : s;
}

/** The folder of a thread opened without a list (by its URL, or from the
 * send receipt): Sent when the person sent the message it shows (the one
 * in focus, else the first), Inbox when that message was addressed to the
 * person, else none. */
export function threadFolder(
  messages: { id: number; from: string; to: string[]; cc: string[] }[],
  me: string | undefined,
  focus?: number,
): Folder | null {
  if (!me) return null;
  const m = messages.find((x) => x.id === focus) ?? messages[0];
  if (!m) return null;
  if (m.from === me) return 'sent';
  if (m.to.includes(me) || m.cc.includes(me)) return 'inbox';
  return null;
}

/** The highlighted section. A thread has no entry of its own: it stays
 * under the list it was opened from (`from`); opened by its URL or after
 * sending, it goes under the folder `guess` (see `threadFolder`), or none. */
export function navSection(section: string, from: string | null, guess: Folder | null): string | null {
  if (section !== 'thread') return section;
  if (from && from !== 'compose') return from;
  return guess;
}
