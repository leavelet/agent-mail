// Delivery states of a message's recipients.

import type { Delivery } from './types';

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

/** A readable notification path: `hook:PostToolUse` -> `PostToolUse hook`. */
export function viaLabel(via: string | undefined): string {
  if (!via) return '';
  const m = /^hook:(.+)$/.exec(via);
  return m ? `${m[1]} hook` : via;
}
