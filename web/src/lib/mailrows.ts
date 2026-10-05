// Rows of message lists, from the different list endpoints.

import { reachedBy } from './delivery';
import type { ArchiveResponse, DeliveryCounts, InboxEntry, MessageHeader, Priority, SearchHit, Source } from './types';

export interface MailRow {
  id: number;
  thread_id: number;
  from: string;
  to: string[];
  cc: string[];
  subject: string;
  created_at: string;
  priority: Priority;
  ack_required: boolean;
  source: Source;
  lines: number;
  meta?: Record<string, string>;
  unread?: boolean;
  acked?: boolean;
  /** How the mail reached the mailbox's seat. */
  cls?: 'to' | 'cc';
  via?: string;
  snippet?: string;
  /** Sent mail: how many of its deliveries are in each state. */
  delivery?: DeliveryCounts;
}

export function fromHeader(m: MessageHeader): MailRow {
  return { ...m };
}

/** Rows of a page of sent mail, with the delivery counts the server sent
 * (`GET /v1/sent?delivery=true`). */
export function fromSent(r: ArchiveResponse): MailRow[] {
  const counts = new Map((r.delivery ?? []).map((c) => [c.message_id, c]));
  return r.messages.map((m) => ({ ...m, delivery: counts.get(m.id) }));
}

export function fromEntry(e: InboxEntry, isList: (address: string) => boolean = () => false): MailRow {
  const reach = reachedBy(e.delivery.seat, e.delivery.class, e.message, isList);
  return {
    ...e.message,
    unread: !e.delivery.read_at,
    acked: !!e.delivery.acked_at,
    cls: reach.cls,
    via: reach.via,
  };
}

export function fromHit(h: SearchHit): MailRow {
  return { ...h };
}

/** Inbox entries come oldest first; lists show newest first. */
export function newestFirst(rows: MailRow[]): MailRow[] {
  return [...rows].sort((a, b) => b.id - a.id);
}
