// Calls of the request and seat control API that the pages share, and how
// their errors are shown.

import { ApiError, enc, get, post, qs } from './api';
import { store } from './store.svelte';
import { t } from './ui.svelte';
import type { Request, RequestDetail, RequestList, SeatControlResponse } from './types';

export function listRequests(params: Record<string, string | number | undefined>): Promise<RequestList> {
  return get<RequestList>(`/v1/requests${qs(params)}`);
}

export function getRequest(num: number): Promise<RequestDetail> {
  return get<RequestDetail>(`/v1/requests/${num}`);
}

export interface AnswerBody {
  option?: string;
  text?: string;
  note?: string;
  if_rev?: number;
}

/** Answer a request. The server marks the user's creation mail of the
 * request read (as for every way a request is closed). */
export async function answerRequest(num: number, body: AnswerBody): Promise<RequestDetail> {
  const r = await post<RequestDetail>(`/v1/requests/${num}/answer`, body);
  void store.requestsChanged(r.request);
  return r;
}

export async function withdrawRequest(num: number, note?: string): Promise<RequestDetail> {
  const r = await post<RequestDetail>(`/v1/requests/${num}/withdraw`, note ? { note } : {});
  void store.requestsChanged(r.request);
  return r;
}

export function promptSeat(name: string, text: string): Promise<SeatControlResponse> {
  return post<SeatControlResponse>(`/v1/seats/${enc(name)}/prompt`, { text });
}

export function interruptSeat(name: string): Promise<SeatControlResponse> {
  return post<SeatControlResponse>(`/v1/seats/${enc(name)}/interrupt`);
}

/** The request a 409 (`answered`, `stale`, `not_open`) carries: as it is now. */
export function conflictRequest(e: unknown): Request | undefined {
  if (!(e instanceof ApiError) || e.status !== 409) return undefined;
  const b = e.body as { request?: Request } | undefined;
  return b?.request;
}

/** An error of a request or seat control call in words. */
export function requestError(e: unknown): string {
  if (e instanceof ApiError) {
    switch (e.code) {
      case 'answered':
        return t('req.err.answered');
      case 'stale':
        return t('req.err.stale');
      case 'not_open':
        return t('req.err.notOpen');
      case 'no_node':
        return t('control.err.noNode');
      case 'no_structured_interrupt':
        return t('control.err.noInterrupt');
      case 'prompt_empty':
        return t('control.err.empty');
      case 'prompt_too_large':
        return t('control.err.tooLarge');
      default:
        return e.message;
    }
  }
  return e instanceof Error ? e.message : String(e);
}
