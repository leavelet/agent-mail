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
  return !!p && (!!p.attention || !!p.wake_failed || !!p.turn_failed);
}

/** Attention tags the interface has words for. */
export const KNOWN_ATTENTION = ['trust-prompt', 'login', 'permission', 'failed', 'wake-failed', 'no-activity', 'turn-failed'];

/** The seat's attention tag: the node's, else `turn-failed` or
 * `wake-failed` (as the server's "needs you" queue names them). */
export function attentionTag(p: Presence | undefined): string | undefined {
  if (!p) return undefined;
  return p.attention || (p.turn_failed ? 'turn-failed' : p.wake_failed ? 'wake-failed' : undefined);
}

/** Prompts a session shows before it starts working. */
const START_PROMPTS = ['trust-prompt', 'login', 'permission'];

/** The node reports the seat offline while its session waits at a prompt
 * for a person (it has not started yet): the interface says "starting". */
export function startingAtPrompt(p: Presence | undefined): boolean {
  return !!p && p.state === 'offline' && !!p.attention && START_PROMPTS.includes(p.attention);
}

/** What the interface shows of a seat's state, in one place: the org row,
 * the seat panel and the "needs you" seat card all take it from here. */
export interface SeatStatus {
  /** The node's report, if it is still valid. */
  presence?: Presence;
  state: SeatState;
  /** Milliseconds in that state, if known. */
  age?: number;
  /** Offline at a prompt for a person: shown as starting. */
  starting: boolean;
  /** Its turns fail. */
  failing: boolean;
  /** The attention tag (see `attentionTag`). */
  tag?: string;
}

export function seatStatus(seat: Seat, now: number): SeatStatus {
  const p = freshPresence(seat.presence, now);
  return {
    presence: p,
    state: p?.state ?? 'offline',
    age: stateAge(seat, now),
    starting: startingAtPrompt(p),
    failing: !!p?.turn_failed,
    tag: attentionTag(p),
  };
}

/** The `.long` text of a status: what the state means. */
export function stateLongKey(s: Pick<SeatStatus, 'state' | 'starting'>): `state.${SeatState | 'starting'}.long` {
  return s.starting ? 'state.starting.long' : `state.${s.state}.long`;
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

/** The zellij session a terminal link opens: its last path segment (the
 * node links a seat to its group's session, not to the seat's tab). */
export function terminalSession(url: string): string {
  try {
    const parts = new URL(url).pathname.split('/').filter(Boolean);
    return decodeURIComponent(parts[parts.length - 1] ?? '');
  } catch {
    return '';
  }
}
