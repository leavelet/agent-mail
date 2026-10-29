// Requests for the person (`r12`): helpers that the "needs you" list, the
// request pages, the inbox and the seat page share. No API calls here (see
// requestops.ts).

import type { AttentionItem, Presence, Request, RequestClass, RequestOption } from './types';

export const CLASSES: RequestClass[] = ['now', 'soon', 'later'];
export const KINDS = ['question', 'plan', 'merge', 'attention', 'permission'] as const;
export const STATES = ['open', 'answered', 'expired', 'withdrawn', 'superseded'] as const;

/** How long Snooze hides a card. */
export const SNOOZE_MS = 3_600_000;

export function requestId(num: number): string {
  return `r${num}`;
}

/** `r12` or `12` as a request number. */
export function parseRequestNum(s: string | undefined | null): number | null {
  const m = /^\s*[rR]?(\d{1,15})\s*$/.exec(s ?? '');
  if (!m) return null;
  const n = Number(m[1]);
  return n > 0 ? n : null;
}

/** The request numbers a subject names as `[r12]`, in order, once each. */
export function subjectRequestTags(subject: string | undefined | null): number[] {
  const out: number[] = [];
  for (const m of (subject ?? '').matchAll(/\[[rR](\d{1,15})\]/g)) {
    const n = Number(m[1]);
    if (n > 0 && !out.includes(n)) out.push(n);
  }
  return out;
}

/** Open requests grouped by class (`now`, `soon`, `later`; empty groups
 * left out), oldest first in each. */
export function groupByClass(reqs: Request[]): { cls: RequestClass; requests: Request[] }[] {
  return CLASSES.map((cls) => ({
    cls,
    requests: reqs
      .filter((r) => r.class === cls)
      .map((r, i) => ({ r, i }))
      .sort((a, b) => Date.parse(a.r.created_at) - Date.parse(b.r.created_at) || a.r.num - b.r.num || a.i - b.i)
      .map((x) => x.r),
  })).filter((g) => g.requests.length > 0);
}

/** The option `key` names. */
export function optionOf(r: Pick<Request, 'options'>, key: string | undefined): RequestOption | undefined {
  if (!key) return undefined;
  return r.options?.find((o) => o.key === key);
}

/** Every request of the group has a default that is one of its options. */
export function allHaveDefault(reqs: Request[]): boolean {
  return reqs.length > 0 && reqs.every((r) => !!optionOf(r, r.default));
}

/** The answer in words: the option's label (or its key), else the text. */
export function answerText(r: Request): string {
  const a = r.answer;
  if (!a) return '';
  if (a.option) return optionOf(r, a.option)?.label ?? a.option;
  return a.text ?? '';
}

/** A message is the creation mail of request `r` (subject `[r12] …`, not a
 * reply, in the request's thread). */
export function isCreationMail(m: { subject: string; thread_id: number }, r: Pick<Request, 'num' | 'thread'>): boolean {
  if (r.thread !== undefined && m.thread_id !== r.thread) return false;
  return m.subject.trimStart().startsWith(`[r${r.num}]`);
}

const REST_KINDS: string[] = ['ack', 'reply', 'seat', 'notice'];

/** The "needs you" queue split: the open requests, and the other entries
 * of the kinds this interface knows, without the creation mail of an open
 * request (its card shows it). */

export function splitAttention(items: AttentionItem[]): { requests: Request[]; rest: AttentionItem[] } {
  const requests = items.filter((i) => i.kind === 'request' && i.request).map((i) => i.request as Request);
  const rest = items.filter((i) => {
    // Kinds this interface does not know (a newer server) are left out
    // rather than shown without words.
    if (!REST_KINDS.includes(i.kind)) return false;
    // A seat entry stays (it shows the condition and how to reach the
    // seat's terminal); `countNeeds` counts it once with its request.
    if (i.kind === 'seat') return true;
    const m = i.message;
    if (m && requests.some((r) => isCreationMail(m, r))) return false;
    return true;
  });
  return { requests, rest };
}

/** How many things wait for the user: the shown requests, and the other
 * entries, where a seat entry with a shown `attention` request of that
 * seat counts once (with the request). */
export function countNeeds(shown: Request[], rest: AttentionItem[]): number {
  const seats = new Set(shown.filter((r) => r.kind === 'attention').map((r) => r.seat));
  return shown.length + rest.filter((i) => !(i.kind === 'seat' && i.seat && seats.has(i.seat))).length;
}

/** Snoozed cards: request number → end of the snooze (ms), kept in this
 * browser. Parse what was stored, dropping what ended. */
export function parseSnoozes(raw: string | null, now: number): Record<string, number> {
  if (!raw) return {};
  try {
    const v = JSON.parse(raw) as unknown;
    if (!v || typeof v !== 'object') return {};
    const out: Record<string, number> = {};
    for (const [k, until] of Object.entries(v as Record<string, unknown>)) {
      if (typeof until === 'number' && until > now && parseRequestNum(k)) out[k] = until;
    }
    return out;
  } catch {
    return {};
  }
}

export function isSnoozed(s: Record<string, number>, num: number, now: number): boolean {
  return (s[String(num)] ?? 0) > now;
}

/** The snoozes with `num` hidden for `ms` from `now` (the ended ones dropped). */
export function addSnooze(s: Record<string, number>, num: number, now: number, ms = SNOOZE_MS): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [k, v] of Object.entries(s)) if (v > now) out[k] = v;
  out[String(num)] = now + ms;
  return out;
}

/** The request a seat sleeps on, if its presence says so. */
export function waitingRequest(p: Pick<Presence, 'waiting' | 'waiting_request'> | undefined): number | undefined {
  if (!p) return undefined;
  if (typeof p.waiting_request === 'number' && p.waiting_request > 0) return p.waiting_request;
  const w = p.waiting;
  if (typeof w === 'number') return w > 0 ? w : undefined;
  if (typeof w === 'string') return parseRequestNum(w) ?? undefined;
  return undefined;
}

/** How a seat's current turn is stopped from the web: Claude Code has no
 * structured interrupt (its terminal is the way), the others do. The
 * harness the node reports comes first, as on the server. */
export function interruptKind(seatHarness: string | undefined, presenceHarness: string | undefined): 'interrupt' | 'terminal' {
  return (presenceHarness || seatHarness) === 'claude' ? 'terminal' : 'interrupt';
}

/** The filters of the requests list page from its query. */
export interface RequestFilters {
  state: string;
  kind: string;
  seat: string;
  cls: string;
}

export function parseFilters(q: URLSearchParams): RequestFilters {
  const state = q.get('state') ?? 'all';
  const kind = q.get('kind') ?? '';
  const cls = q.get('class') ?? '';
  return {
    state: state === 'all' || (STATES as readonly string[]).includes(state) ? state : 'all',
    kind: (KINDS as readonly string[]).includes(kind) ? kind : '',
    seat: q.get('seat') ?? '',
    cls: (CLASSES as string[]).includes(cls) ? cls : '',
  };
}
