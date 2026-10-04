// Seat presence as the interface shows it.

import type { Presence, Seat, SeatState } from './types';

/** The server drops presence not refreshed for this long without telling anyone. */
export const PRESENCE_TTL_MS = 90_000;

export function parseTime(s: string | undefined | null): number {
  if (!s) return NaN;
  return Date.parse(s);
}

/** Presence that is still valid at `now`, else undefined. */
/** The end of the seat's focus, if it is in focus at `now`. */
export function focusUntil(seat: Seat, now: number): string | undefined {
  const t = parseTime(seat.focus_until);
  return t > now ? seat.focus_until : undefined;
}

export function freshPresence(p: Presence | undefined, now: number): Presence | undefined {
  if (!p) return undefined;
  const updated = parseTime(p.updated_at);
  if (Number.isNaN(updated) || now - updated > PRESENCE_TTL_MS) return undefined;
  return p;
}

export function effectiveState(seat: Seat, now: number): SeatState {
  return freshPresence(seat.presence, now)?.state ?? 'offline';
}

/** Milliseconds the seat has been in its state, if known. */
export function stateAge(seat: Seat, now: number): number | undefined {
  const p = freshPresence(seat.presence, now);
  if (!p) return undefined;
  const since = parseTime(p.since);
  return Number.isNaN(since) ? undefined : Math.max(0, now - since);
}

export function needsPerson(seat: Seat, now: number): boolean {
  const p = freshPresence(seat.presence, now);
  return !!p && (!!p.attention || !!p.wake_failed);
}

/** Why node operations (sleep, wake) are unavailable, or null if they are. */
export function nodeOpBlock(seat: Seat, now: number): 'user' | 'noPresence' | 'noNode' | null {
  if (seat.kind === 'user') return 'user';
  const p = freshPresence(seat.presence, now);
  if (!p) return 'noPresence';
  if (p.node_connected === false) return 'noNode';
  return null;
}

/** A seat's terminal link, only if it is an http(s) URL (the server drops
 * others; this guards older servers). */
export function terminalHref(url: string | undefined): string | undefined {
  if (!url) return undefined;
  try {
    const u = new URL(url);
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.href : undefined;
  } catch {
    return undefined;
  }
}
