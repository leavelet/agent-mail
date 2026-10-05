// Delivery states of a message's recipients.

import type { Delivery, DeliveryCounts } from './types';

export type DeliveryState = 'queued' | 'notified' | 'read' | 'acked';

export function deliveryState(d: Pick<Delivery, 'notified_at' | 'read_at' | 'acked_at'>): DeliveryState {
  if (d.acked_at) return 'acked';
  if (d.read_at) return 'read';
  if (d.notified_at) return 'notified';
  return 'queued';
}

/** Steps reached, for a progress display: queued(0) .. acked(3). */
export function deliveryStep(s: DeliveryState): number {
  return ['queued', 'notified', 'read', 'acked'].indexOf(s);
}

/** The states a message's deliveries are in, most advanced first, with
 * how many deliveries are in each (states without any left out). */
export function deliverySummary(c: Pick<DeliveryCounts, DeliveryState>): { state: DeliveryState; n: number }[] {
  return (['acked', 'read', 'notified', 'queued'] as const).map((state) => ({ state, n: c[state] ?? 0 })).filter((x) => x.n > 0);
}

/** A readable notification path: `hook:PostToolUse` -> `PostToolUse hook`. */
export function viaLabel(via: string | undefined): string {
  if (!via) return '';
  const m = /^hook:(.+)$/.exec(via);
  return m ? `${m[1]} hook` : via;
}

/** How a message reached a seat: the header field (`to` or `cc`) and, when
 * the seat is not named there itself, the list it came through. The class
 * of a delivery follows the header field (a list in To gives `to`); older
 * deliveries may carry class `list`, which is shown like `cc`. */
export interface Reach {
  cls: 'to' | 'cc';
  via?: string;
}

export function reachedBy(
  seat: string,
  cls: string,
  m: { to: string[]; cc: string[] },
  isList: (address: string) => boolean,
): Reach {
  if (m.to.includes(seat)) return { cls: 'to' };
  if (m.cc.includes(seat)) return { cls: 'cc' };
  const field = cls === 'to' ? m.to : cls === 'cc' ? m.cc : [...m.to, ...m.cc];
  const lists = field.filter(isList);
  const via = lists[0] ?? field.find((a) => a !== seat);
  return { cls: cls === 'to' ? 'to' : 'cc', via };
}
