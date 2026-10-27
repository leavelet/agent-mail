// Documents in the web interface: ids, references, anchors, filters and
// the other pure helpers the document pages share.

import type { Doc, DocStatus, DocSection } from './types';

export const DOC_STATUSES: DocStatus[] = ['draft', 'active', 'superseded', 'archived'];

/** `d12`. */
export const docId = (num: number) => `d${num}`;

/** A document number from `12`, `d12` or `D12` (null otherwise). */
export function parseDocNum(s: string): number | null {
  const m = /^\s*[dD]?(\d{1,15})\s*$/.exec(s);
  if (!m) return null;
  const n = Number(m[1]);
  return n > 0 ? n : null;
}

/** What names a document in a URL or an input: a number (`d12`) or a slug,
 * with an optional section anchor (`d12#rollback`, `runbook.deploy#steps`). */
export interface DocTarget {
  /** `d12`, or the slug as written. */
  ref: string;
  num: number | null;
  anchor: string;
}

export function parseDocTarget(s: string): DocTarget {
  const i = s.indexOf('#');
  const head = (i >= 0 ? s.slice(0, i) : s).trim();
  const anchor = i >= 0 ? s.slice(i + 1).trim() : '';
  const num = parseDocNum(head);
  return { ref: num ? docId(num) : head.toLowerCase(), num, anchor };
}

/** The path segment the API takes for a document: its number, else the slug. */
export function apiRef(t: Pick<DocTarget, 'ref' | 'num'>): string {
  return t.num ? String(t.num) : t.ref;
}

/** The hash route of a document, with a section. */
export function docHref(num: number | string, anchor?: string): string {
  const id = typeof num === 'number' ? docId(num) : num;
  return `#/docs/${encodeURIComponent(id)}${anchor ? `#${encodeURIComponent(anchor)}` : ''}`;
}

/** The colour of a status chip. */
export function docStatusTone(s: DocStatus | string): 'ok' | 'accent' | 'warn' | 'neutral' | 'notice' {
  switch (s) {
    case 'active':
      return 'ok';
    case 'draft':
      return 'warn';
    case 'superseded':
      return 'notice';
    default:
      return 'neutral';
  }
}

/** A heading's anchor derived from its text, as the server derives it
 * (GitHub style: lower case, letters of any script, digits, `-` and `_`
 * kept, white space to `-`, the rest dropped). Only a fallback: the
 * viewer takes the anchors from the server's section list. */
export function slugify(text: string): string {
  let out = '';
  for (const c of text.trim()) {
    if (/[\p{Alphabetic}\p{N}_-]/u.test(c)) out += c.toLowerCase();
    else if (/\s/u.test(c)) out += '-';
  }
  return out;
}

/** The sections that have a heading (the text before the first heading
 * has none), in body order. */
export function headedSections(sections: DocSection[]): DocSection[] {
  return sections.filter((s) => s.level > 0).sort((a, b) => a.start - b.start);
}

/** A body with the kind's required sections as headings (for a new
 * document of that kind). */
export function bodyTemplate(sections: string[]): string {
  return sections.map((s) => `## ${s}\n\n`).join('');
}

/** Labels or seat names typed as `a, b c`. */
export function parseList(s: string): string[] {
  return [
    ...new Set(
      s
        .split(/[,\s]+/)
        .map((x) => x.trim())
        .filter(Boolean),
    ),
  ];
}

/** The filters of a document list, as the URL carries them. */
export interface DocFilters {
  kind: string;
  /** `''` (the server's default: draft and active), one status, or `all`. */
  status: string;
  team: string;
  owner: string;
  label: string;
  stale: boolean;
  pinned: boolean;
}

export function filtersFrom(q: URLSearchParams): DocFilters {
  return {
    kind: q.get('kind') ?? '',
    status: q.get('status') ?? '',
    team: q.get('team') ?? '',
    owner: q.get('owner') ?? '',
    label: q.get('label') ?? '',
    stale: q.get('stale') === '1',
    pinned: q.get('pinned') === '1',
  };
}

/** The query of `GET /v1/docs` (and of the page's URL) for filters. */
export function filterQuery(f: DocFilters): Record<string, string | undefined> {
  return {
    kind: f.kind || undefined,
    status: f.status || undefined,
    team: f.team || undefined,
    owner: f.owner || undefined,
    label: f.label || undefined,
    stale: f.stale ? '1' : undefined,
    pinned: f.pinned ? '1' : undefined,
  };
}

/** What the landing page of a team shows, from its documents (draft and
 * active, newest first). */
export interface DocSummary {
  pinned: Doc[];
  kinds: Map<string, number>;
  stale: number;
  drafts: number;
  total: number;
}

export function summarize(docs: Doc[]): DocSummary {
  const kinds = new Map<string, number>();
  let stale = 0;
  let drafts = 0;
  for (const d of docs) {
    kinds.set(d.kind, (kinds.get(d.kind) ?? 0) + 1);
    if (d.stale) stale++;
    if (d.status === 'draft') drafts++;
  }
  return { pinned: docs.filter((d) => d.pinned), kinds, stale, drafts, total: docs.length };
}

/** One line of a unified diff with its role. */
export interface DiffLine {
  kind: 'add' | 'del' | 'hunk' | 'head' | 'ctx';
  text: string;
}

export function diffLines(diff: string): DiffLine[] {
  const lines = diff.split('\n');
  if (lines.length && lines[lines.length - 1] === '') lines.pop();
  return lines.map((text) => {
    if (text.startsWith('+++') || text.startsWith('---') || /^(title|description):/.test(text)) return { kind: 'head', text };
    if (text.startsWith('@@')) return { kind: 'hunk', text };
    if (text.startsWith('+')) return { kind: 'add', text };
    if (text.startsWith('-')) return { kind: 'del', text };
    return { kind: 'ctx', text };
  });
}

/** Verification intervals offered in the viewer (`''`: the kind's default). */
export const VERIFY_INTERVALS = ['', '7d', '30d', '60d', '90d', 'none'] as const;

/** Error codes of the document and handover API that have their own text. */
export const DOC_ERRORS = [
  'stale',
  'frozen',
  'slug_taken',
  'not_owner',
  'sections_missing',
  'body_too_large',
  'nothing_to_split',
  'no_section',
  'invalid_transition',
  'invalid_relation',
  'parent_cycle',
  'parent_too_deep',
  'unknown_kind',
  'kind_disabled',
  'invalid_slug',
  'invalid_label',
  'too_many_labels',
  'too_many_editors',
  'invalid_interval',
  'note_too_large',
  'note_fields_missing',
  'not_an_agent',
  'private',
  'storage_quota',
  'not_found',
  'rate_limited',
] as const;
export type DocErrorCode = (typeof DOC_ERRORS)[number];

export function docErrorCode(status: number, code: string): DocErrorCode | null {
  if ((DOC_ERRORS as readonly string[]).includes(code)) return code as DocErrorCode;
  if (status === 404) return 'not_found';
  if (status === 429) return 'rate_limited';
  return null;
}

// --- handover --------------------------------------------------------------------

/** The fields of a handover note, in the order the server keeps them. */
export const HANDOVER_FIELDS = ['state', 'summary', 'next', 'waiting_on', 'traps', 'verify', 'pointers'] as const;
export type HandoverField = (typeof HANDOVER_FIELDS)[number];

/** The labels the server reads in a note's text. */
export const HANDOVER_LABELS: Record<HandoverField, string> = {
  state: 'State',
  summary: 'Summary',
  next: 'Next',
  waiting_on: 'Waiting on',
  traps: 'Traps',
  verify: 'Verify',
  pointers: 'Pointers',
};

/** An empty note as text, one heading per field, for the editor. */
export function handoverTemplate(): string {
  return HANDOVER_FIELDS.map((f) => `## ${HANDOVER_LABELS[f]}\n\n`).join('');
}

/** Worklog `since` choices (`''`: no bound). */
export const WORKLOG_SINCE = ['', '1h', '6h', '24h', '7d', '30d'] as const;
