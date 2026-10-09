// Browser notifications for the "needs you" queue: after the user turned
// them on (and the browser granted permission), each new item in the queue
// raises one system notification while the page is in the background.

import { load, save } from './storage';
import type { AttentionItem } from './types';
import { shortSubject } from './subject';

const KEY = 'amail.notify';

/** A stable key for an attention item. */
export function itemKey(i: AttentionItem): string {
  if (i.message) return `${i.kind}:m${i.message.id}`;
  return `${i.kind}:s${i.seat ?? ''}:${i.attention ?? ''}`;
}

/**
 * Items of `items` that were not in `seen`. With `seen === null` (the first
 * load) nothing is new: only what arrives later is announced.
 */
export function newItems(seen: Set<string> | null, items: AttentionItem[]): AttentionItem[] {
  if (seen === null) return [];
  return items.filter((i) => !seen.has(itemKey(i)));
}

export type Permission = 'default' | 'granted' | 'denied' | 'unsupported';

export function permission(): Permission {
  const N = (globalThis as { Notification?: { permission: NotificationPermission } }).Notification;
  return N ? N.permission : 'unsupported';
}

/** The user's choice, kept in this browser. Off until turned on. */
export function enabled(): boolean {
  return load(KEY) === 'on';
}

export function setEnabled(on: boolean): void {
  save(KEY, on ? 'on' : null);
}

/** Ask the browser for permission; true when granted. */
export async function requestPermission(): Promise<boolean> {
  const N = (globalThis as { Notification?: typeof Notification }).Notification;
  if (!N) return false;
  if (N.permission === 'granted') return true;
  if (N.permission === 'denied') return false;
  return (await N.requestPermission()) === 'granted';
}

/** Title and body of the notification for an item. */
export function describe(
  i: AttentionItem,
  t: (k: 'notify.ack' | 'notify.reply' | 'notify.seat' | 'notify.notice' | 'mail.writtenBy', p?: Record<string, string>) => string,
): { title: string; body: string } {
  if (i.kind === 'seat') return { title: t('notify.seat', { seat: i.seat ?? '' }), body: i.hint ?? i.attention ?? '' };
  const m = i.message;
  const from = m?.written_by ? `${m.from} (${t('mail.writtenBy', { user: m.written_by })})` : (m?.from ?? '');
  const title = t(i.kind === 'ack' ? 'notify.ack' : i.kind === 'notice' ? 'notify.notice' : 'notify.reply', { from });
  return { title, body: shortSubject(m?.subject ?? '') };
}

/** Show notifications for new items when enabled, permitted and the page is in the background. */
export function announce(items: AttentionItem[], t: Parameters<typeof describe>[1], onClick: (i: AttentionItem) => void): number {
  if (!items.length || !enabled() || permission() !== 'granted') return 0;
  const doc = globalThis.document;
  if (doc && doc.visibilityState === 'visible' && doc.hasFocus()) return 0;
  let n = 0;
  for (const i of items.slice(0, 5)) {
    const { title, body } = describe(i, t);
    try {
      const note = new Notification(title, { body, tag: itemKey(i) });
      note.onclick = () => {
        globalThis.focus?.();
        onClick(i);
        note.close();
      };
      n++;
    } catch {
      // Some browsers only allow notifications from a service worker.
    }
  }
  return n;
}
