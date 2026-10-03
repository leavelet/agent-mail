// The "needs you" queue: helpers over GET /v1/attention.

import type { AttentionItem } from './types';

/** Local seats that have mail waiting for the user (an org-tree marker). */
export function seatsWaitingForUser(items: AttentionItem[]): Set<string> {
  const out = new Set<string>();
  for (const it of items) {
    if ((it.kind === 'ack' || it.kind === 'reply') && it.seat && !it.seat.includes('@')) out.add(it.seat);
  }
  return out;
}

export function isUrgent(it: AttentionItem): boolean {
  return it.message?.priority === 'urgent';
}

/** Group rank used by the server's ordering: urgent mail, seats, other mail. */
export function attentionRank(it: AttentionItem): number {
  if (it.kind !== 'seat' && isUrgent(it)) return 0;
  if (it.kind === 'seat') return 1;
  return 2;
}

/** Sort like the server (stable): rank, then oldest first. */
export function sortAttention(items: AttentionItem[]): AttentionItem[] {
  return items
    .map((it, i) => ({ it, i }))
    .sort((a, b) => {
      const r = attentionRank(a.it) - attentionRank(b.it);
      if (r) return r;
      const t = Date.parse(a.it.since) - Date.parse(b.it.since);
      return t || a.i - b.i;
    })
    .map((x) => x.it);
}

/** A stable key for an item (for selection across refreshes). */
export function attentionKey(it: AttentionItem): string {
  return it.kind === 'seat' ? `seat:${it.seat}` : `msg:${it.message?.id}`;
}
