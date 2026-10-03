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
  kind: 'seat' | 'list' | 'org';
  detail?: string;
}

/** Suggestions for the token being typed: seats, lists, then `name@org` for linked orgs. */
export function suggest(
  token: string,
  seats: { name: string; detail?: string }[],
  lists: string[],
  orgs: string[],
  limit = 8,
): Suggestion[] {
  const [name, org] = token.split('@', 2);
  const out: Suggestion[] = [];
  if (org === undefined) {
    const rank = (n: string) => (n.startsWith(name) ? 0 : n.includes(name) ? 1 : 2);
    const seatHits = seats.filter((s) => rank(s.name) < 2).sort((a, b) => rank(a.name) - rank(b.name) || a.name.localeCompare(b.name));
    for (const s of seatHits) out.push({ address: s.name, kind: 'seat', detail: s.detail });
    for (const l of lists.filter((l) => rank(l) < 2).sort((a, b) => rank(a) - rank(b) || a.localeCompare(b)))
      out.push({ address: l, kind: 'list' });
  }
  if (name) {
    for (const o of orgs) {
      if (org === undefined || o.startsWith(org)) out.push({ address: `${name}@${o}`, kind: 'org', detail: o });
    }
  }
  return out.slice(0, limit);
}
