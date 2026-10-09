// A thin client for the amail API. The session cookie authenticates; every
// request carries the CSRF header the server requires for cookie auth.

import type { ApiErrorBody } from './types';
import { apiUrl } from './base';

export const CSRF_HEADER = 'X-Amail-Csrf';

export class ApiError extends Error {
  status: number;
  code: string;
  rejected: { address: string; reason: string }[];
  constructor(status: number, code: string, message: string, rejected: ApiError['rejected'] = []) {
    super(message);
    this.status = status;
    this.code = code;
    this.rejected = rejected;
  }
}

let onUnauthorized: (() => void) | null = null;

/** Called when any request comes back 401 (the session ended). */
export function setUnauthorizedHandler(f: (() => void) | null): void {
  onUnauthorized = f;
}

/** `path` is an API path (`/v1/...`); it is sent below the app's path prefix. */
export async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { [CSRF_HEADER]: '1', Accept: 'application/json' };
  const init: RequestInit = { method, headers, credentials: 'same-origin' };
  if (body !== undefined) {
    headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  let resp: Response;
  try {
    resp = await fetch(apiUrl(path), init);
  } catch (e) {
    throw new ApiError(0, 'network', e instanceof Error ? e.message : String(e));
  }
  const text = await resp.text();
  let data: unknown = undefined;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = text;
    }
  }
  if (!resp.ok) {
    const err = (data as ApiErrorBody | undefined)?.error;
    if (resp.status === 401 && path !== '/v1/session') onUnauthorized?.();
    throw new ApiError(
      resp.status,
      err?.code ?? 'http',
      err?.message ?? `HTTP ${resp.status}`,
      err?.rejected ?? [],
    );
  }
  return data as T;
}

export const get = <T>(path: string) => request<T>('GET', path);
export const post = <T>(path: string, body?: unknown) => request<T>('POST', path, body ?? {});
export const patch = <T>(path: string, body: unknown) => request<T>('PATCH', path, body);
export const put = <T>(path: string, body?: unknown) => request<T>('PUT', path, body ?? {});
export const del = <T>(path: string) => request<T>('DELETE', path);

/** Build a query string from defined values. */
export function qs(params: Record<string, string | number | boolean | undefined | null>): string {
  const parts: string[] = [];
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null || v === '') continue;
    parts.push(`${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`);
  }
  return parts.length ? `?${parts.join('&')}` : '';
}

export const enc = encodeURIComponent;
