// Parsing and validation for the compose form.

import type { Meta, SendRequest } from './types';

const NAME = '[a-z0-9][a-z0-9._-]{0,62}';
const ADDRESS = new RegExp(`^${NAME}(@${NAME})?$`);
const META_KEY = /^[a-z0-9_.-]{1,32}$/;

export function isAddress(s: string): boolean {
  return ADDRESS.test(s);
}

/** Split a comma/space separated address field into trimmed, de-duplicated addresses. */
export function parseAddresses(s: string): string[] {
  const out: string[] = [];
  for (const part of s.split(/[\s,;]+/)) {
    const a = part.trim().toLowerCase();
    if (a && !out.includes(a)) out.push(a);
  }
  return out;
}

export interface MetaRow {
  key: string;
  value: string;
}

export type ComposeError =
  | { kind: 'noTo' }
  | { kind: 'addr'; a: string }
  | { kind: 'metaKey'; k: string }
  | { kind: 'metaValue'; k: string }
  | { kind: 'metaDup'; k: string }
  | { kind: 'subject' };

export function parseMeta(rows: MetaRow[]): { meta: Meta; error?: ComposeError } {
  const meta: Meta = {};
  for (const r of rows) {
    const k = r.key.trim();
    const v = r.value.trim();
    if (!k && !v) continue;
    if (!META_KEY.test(k)) return { meta, error: { kind: 'metaKey', k } };
    if ([...v].some((c) => c.charCodeAt(0) < 0x20 || c.charCodeAt(0) === 0x7f)) {
      return { meta, error: { kind: 'metaValue', k } };
    }
    if (k in meta) return { meta, error: { kind: 'metaDup', k } };
    meta[k] = v;
  }
  return { meta };
}

export interface ComposeInput {
  to: string;
  cc: string;
  subject: string;
  body: string;
  urgent: boolean;
  ack: boolean;
  meta: MetaRow[];
  inReplyTo?: number;
}

export function buildSend(input: ComposeInput): { req?: SendRequest; error?: ComposeError } {
  const to = parseAddresses(input.to);
  const cc = parseAddresses(input.cc);
  if (to.length === 0 && cc.length === 0) return { error: { kind: 'noTo' } };
  for (const a of [...to, ...cc]) if (!isAddress(a)) return { error: { kind: 'addr', a } };
  if (/[\r\n]/.test(input.subject)) return { error: { kind: 'subject' } };
  const { meta, error } = parseMeta(input.meta);
  if (error) return { error };
  const req: SendRequest = { to, cc, subject: input.subject.trim(), body: input.body };
  if (input.inReplyTo !== undefined) req.in_reply_to = input.inReplyTo;
  if (input.urgent) req.priority = 'urgent';
  if (input.ack) req.ack_required = true;
  if (Object.keys(meta).length) req.meta = meta;
  return { req };
}

/** "Re: " once. */
export function replySubject(s: string): string {
  return /^re:\s/i.test(s) ? s : `Re: ${s}`;
}

/** The address being typed at the end of a field, for autocompletion. */
export function currentToken(field: string): string {
  const m = /([^\s,;]*)$/.exec(field);
  return (m ? m[1] : '').toLowerCase();
}

/** Replace the token being typed with `addr`, followed by ", ". */
export function completeToken(field: string, addr: string): string {
  return field.replace(/([^\s,;]*)$/, '') + addr + ', ';
}

export interface Suggestion {
  address: string;
  /** `open`: a seat of another org open to this org; `participant`: a seat
   * of another org that wrote in the thread being answered. */
  kind: 'seat' | 'list' | 'open' | 'participant';
  detail?: string;
}

/**
 * Suggestions for the token being typed: seats and lists of the org, then
 * addresses of other orgs from `external` (only what other orgs opened to
 * this org and the participants of the thread being answered; see
 * `peerCandidates`). A token with `@` matches only addresses of other orgs.
 */
export function suggest(
  token: string,
  seats: { name: string; detail?: string }[],
  lists: string[],
  external: { address: string; kind: 'open' | 'participant'; detail?: string }[],
  limit = 8,
): Suggestion[] {
  const out: Suggestion[] = [];
  const rank = (n: string, q: string) => (n.startsWith(q) ? 0 : n.includes(q) ? 1 : 2);
  if (!token.includes('@')) {
    const seatHits = seats.filter((s) => rank(s.name, token) < 2).sort((a, b) => rank(a.name, token) - rank(b.name, token) || a.name.localeCompare(b.name));
    for (const s of seatHits) out.push({ address: s.name, kind: 'seat', detail: s.detail });
    for (const l of lists.filter((l) => rank(l, token) < 2).sort((a, b) => rank(a, token) - rank(b, token) || a.localeCompare(b)))
      out.push({ address: l, kind: 'list' });
  }
  const ext = external
    .filter((e) => rank(e.address, token) < 2)
    .sort((a, b) => rank(a.address, token) - rank(b.address, token) || a.address.localeCompare(b.address));
  for (const e of ext) out.push({ address: e.address, kind: e.kind, detail: e.detail });
  return out.slice(0, limit);
}
