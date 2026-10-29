// Mail actions shared by the inbox, the queue and the thread view.

import { post, enc } from './api';
import { store } from './store.svelte';
import type { Delivery } from './types';

export async function markRead(id: number): Promise<Delivery> {
  const r = await post<{ delivery: Delivery }>(`/v1/messages/${enc(String(id))}/read`);
  void store.mailChanged();
  return r.delivery;
}

/** Mark the user's delivery not read again (acknowledged mail stays read). */
export async function markUnread(id: number): Promise<Delivery> {
  const r = await post<{ delivery: Delivery }>(`/v1/messages/${enc(String(id))}/unread`);
  void store.mailChanged();
  return r.delivery;
}

/** Mark the user's deliveries of `ids` read in one request (`POST
 * /v1/inbox/read`); ids that are not the user's unread deliveries are
 * skipped. Returns how many were marked. */
export async function markReadMany(ids: number[]): Promise<number> {
  if (!ids.length) return 0;
  const r = await post<{ marked: number }>('/v1/inbox/read', { ids });
  void store.mailChanged();
  return r.marked;
}

/** Mark every unread delivery of the user read; with `beforeSeq`, only
 * those with a smaller seq (mail that arrived after the list was loaded
 * stays unread). Returns how many were marked. */
export async function markAllRead(beforeSeq?: number): Promise<number> {
  const body = beforeSeq === undefined ? { all: true } : { all: true, before_seq: beforeSeq };
  const r = await post<{ marked: number }>('/v1/inbox/read', body);
  void store.mailChanged();
  return r.marked;
}

/** Mark the user's deliveries of `ids` not read again, `parallel` requests
 * at a time (the server has no bulk route for this). Returns how many
 * succeeded; after all were tried, the first failure is thrown. */
export async function markUnreadMany(ids: number[], parallel = 6): Promise<number> {
  let done = 0;
  let failure: unknown = undefined;
  for (let i = 0; i < ids.length; i += parallel) {
    const part = ids.slice(i, i + parallel);
    const results = await Promise.allSettled(part.map((id) => post(`/v1/messages/${enc(String(id))}/unread`)));
    for (const r of results) {
      if (r.status === 'fulfilled') done++;
      else failure ??= r.reason;
    }
  }
  void store.mailChanged();
  if (failure !== undefined) throw failure;
  return done;
}

export async function ack(id: number): Promise<Delivery> {
  const r = await post<{ delivery: Delivery }>(`/v1/messages/${enc(String(id))}/ack`);
  void store.mailChanged();
  return r.delivery;
}
