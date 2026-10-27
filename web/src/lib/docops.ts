// Calls of the document and handover API that the pages share, and how
// their errors are shown.

import { ApiError, get, patch, post, put, qs, enc } from './api';
import { docErrorCode } from './docs';
import { t } from './ui.svelte';
import type { DocDetail, DocList, DocRef, DocStaleBody, Doc } from './types';

/** `/v1/docs/{ref}{rest}`; `ref` is a number or a slug. */
export const docPath = (ref: number | string, rest = '') => `/v1/docs/${enc(String(ref))}${rest}`;

export function getDoc(ref: number | string): Promise<DocDetail> {
  return get<DocDetail>(docPath(ref));
}

export function patchDoc(num: number, body: Record<string, unknown>): Promise<DocDetail> {
  return patch<DocDetail>(docPath(num), body);
}

export function writeBody(num: number, body: Record<string, unknown>): Promise<DocDetail> {
  return put<DocDetail>(docPath(num, '/body'), body);
}

export function docAction(num: number, action: 'supersede' | 'verify' | 'split' | 'extract' | 'merge', body: Record<string, unknown> = {}): Promise<DocDetail> {
  return post<DocDetail>(docPath(num, `/${action}`), body);
}

export function listDocs(params: Record<string, string | number | undefined>): Promise<DocList> {
  return get<DocList>(`/v1/docs${qs(params)}`);
}

/** Documents whose title, slug or description contains every word. */
export async function findDocs(q: string, params: Record<string, string | number | undefined> = {}): Promise<Doc[]> {
  if (!q.trim()) return [];
  return (await get<{ docs: Doc[] }>(`/v1/docs/find${qs({ q: q.trim(), ...params })}`)).docs;
}

/** Similar documents for a title being typed: the documents whose title
 * contains every word, else those that contain one of its longer words
 * (`GET /docs/find` asks for every word; the create answer's own
 * candidates come only after the create). */
export async function titleCandidates(title: string, kind: string, max = 5): Promise<DocRef[]> {
  const words = title
    .toLowerCase()
    .split(/[^\p{L}\p{N}_.-]+/u)
    .filter(Boolean);
  if (!words.length) return [];
  const out = new Map<number, DocRef>();
  for (const d of await findDocs(words.join(' '), { limit: max })) out.set(d.num, d);
  if (out.size < max) {
    const long = [...new Set(words.filter((w) => [...w].length >= 4))].sort((a, b) => b.length - a.length).slice(0, 3);
    if (long.length > 1 || (long.length === 1 && words.length > 1)) {
      const lists = await Promise.all(long.map((w) => findDocs(w, { limit: max }).catch(() => [] as Doc[])));
      for (const l of lists) for (const d of l) if (out.size < max * 2) out.set(d.num, d);
    }
  }
  // The same kind first, as the server's candidates are.
  return [...out.values()].sort((a, b) => Number(b.kind === kind) - Number(a.kind === kind)).slice(0, max);
}

/** The answer of the last create, for the viewer it opens (it shows the
 * similar documents and dangling links the server found). */
export const lastCreated: { detail: DocDetail | null } = { detail: null };

/** The body of a 409 `stale` answer, if `e` is one. */
export function staleBody(e: unknown): DocStaleBody | null {
  if (e instanceof ApiError && e.status === 409 && e.code === 'stale' && e.body && typeof e.body === 'object' && 'diff' in e.body) {
    return e.body as DocStaleBody;
  }
  return null;
}

/** An error of a document or handover call in words: the known codes have
 * their own text, others show the server's message and code. */
export function docError(e: unknown): string {
  if (e instanceof ApiError) {
    const code = docErrorCode(e.status, e.code);
    if (code) return `${t(`doc.err.${code}`)} (${e.message})`;
    return `${e.message} (${e.code})`;
  }
  return e instanceof Error ? e.message : String(e);
}
