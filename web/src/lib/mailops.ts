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

export async function ack(id: number): Promise<Delivery> {
  const r = await post<{ delivery: Delivery }>(`/v1/messages/${enc(String(id))}/ack`);
  void store.mailChanged();
  return r.delivery;
}
