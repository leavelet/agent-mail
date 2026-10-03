// Rows of message lists, from the different list endpoints.

import type { DeliveryClass, InboxEntry, MessageHeader, Priority, SearchHit, Source } from './types';

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
  cls?: DeliveryClass;
  snippet?: string;
}

export function fromHeader(m: MessageHeader): MailRow {
  return { ...m };
}

export function fromEntry(e: InboxEntry): MailRow {
  return {
    ...e.message,
    unread: !e.delivery.read_at,
    acked: !!e.delivery.acked_at,
    cls: e.delivery.class,
  };
}

export function fromHit(h: SearchHit): MailRow {
  return { ...h };
}

/** Inbox entries come oldest first; lists show newest first. */
export function newestFirst(rows: MailRow[]): MailRow[] {
  return [...rows].sort((a, b) => b.id - a.id);
}
