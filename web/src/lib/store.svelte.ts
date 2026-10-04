// Shared live state: who is signed in, the seats with their presence and
// mailbox counters, the "needs you" queue and the org summary. Live updates
// come from the server's event stream and are merged into about one update
// per second.

import { ApiError, get, post, del, qs, enc, setUnauthorizedHandler } from './api';
import { Coalescer } from './coalesce';
import type {
  AttentionItem,
  CountsResponse,
  Link,
  ListInfo,
  MailboxEvent,
  Overview,
  PresenceEvent,
  Seat,
  SeatCounts,
  WhoAmI,
} from './types';

export type AuthState = 'checking' | 'out' | 'in';

const FLUSH_MS = 1000;
const OVERVIEW_MIN_MS = 5000;
const ALL_COUNTS_OVER = 25;

class Store {
  auth = $state<AuthState>('checking');
  me = $state.raw<WhoAmI | null>(null);
  seats = $state.raw<Seat[]>([]);
  counts = $state.raw<Map<string, SeatCounts>>(new Map());
  overview = $state.raw<Overview | null>(null);
  attention = $state.raw<AttentionItem[]>([]);
  lists = $state.raw<ListInfo[]>([]);
  links = $state.raw<Link[]>([]);
  /** The event stream is open. */
  live = $state(false);
  loadError = $state<string | null>(null);

  seatMap = $derived(new Map(this.seats.map((s) => [s.name, s])));

  private es: EventSource | null = null;
  private presence = new Map<string, PresenceEvent>();
  private coalescer = new Coalescer<string>(FLUSH_MS, (keys) => void this.flush(keys));
  private lastOverview = 0;
  private overviewTimer: ReturnType<typeof setTimeout> | null = null;
  private timers: ReturnType<typeof setInterval>[] = [];

  constructor() {
    setUnauthorizedHandler(() => this.signedOut());
  }

  async check(): Promise<void> {
    try {
      const me = await get<WhoAmI>('/v1/whoami');
      if (me.scope !== 'user') {
        this.signedOut();
        return;
      }
      this.me = me;
      this.auth = 'in';
      await this.start();
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) this.signedOut();
      else {
        this.loadError = e instanceof Error ? e.message : String(e);
        this.auth = 'out';
      }
    }
  }

  async login(token: string): Promise<void> {
    const me = await post<WhoAmI>('/v1/session', { token });
    this.me = me;
    this.auth = 'in';
    await this.start();
  }

  async logout(): Promise<void> {
    try {
      await del('/v1/session');
    } catch {
      // Signed out either way.
    }
    this.signedOut();
  }

  private signedOut() {
    this.stop();
    this.me = null;
    this.auth = 'out';
  }

  private async start() {
    this.loadError = null;
    await Promise.all([this.refreshSeats(), this.refreshCounts(), this.refreshAttention(), this.refreshOverview(), this.refreshDirectory()]).catch(
      (e) => (this.loadError = e instanceof Error ? e.message : String(e)),
    );
    this.connect();
    this.timers.push(setInterval(() => void this.refreshSeats(), 60_000));
    this.timers.push(setInterval(() => void this.refreshOverview(), 30_000));
  }

  private stop() {
    this.es?.close();
    this.es = null;
    this.live = false;
    this.coalescer.cancel();
    for (const t of this.timers) clearInterval(t);
    this.timers = [];
  }

  private connect() {
    if (typeof EventSource === 'undefined') return;
    this.es?.close();
    const es = new EventSource('/v1/events');
    this.es = es;
    es.onopen = () => {
      this.live = true;
    };
    es.addEventListener('mailbox', (e) => {
      try {
        const m = JSON.parse((e as MessageEvent).data) as MailboxEvent;
        this.coalescer.add(`m:${m.seat}`);
      } catch {
        // Ignore malformed events.
      }
    });
    es.addEventListener('presence', (e) => {
      try {
        const p = JSON.parse((e as MessageEvent).data) as PresenceEvent;
        this.presence.set(p.seat, p);
        this.coalescer.add(`p:${p.seat}`);
      } catch {
        // Ignore malformed events.
      }
    });
    es.onerror = () => {
      this.live = false;
      if (es.readyState === EventSource.CLOSED && this.es === es) {
        // The browser gave up (e.g. the session ended): check, then retry.
        setTimeout(() => {
          if (this.es !== es || this.auth !== 'in') return;
          void get<WhoAmI>('/v1/whoami')
            .then(() => {
              this.connect();
              void this.resync();
            })
            .catch(() => undefined);
        }, 3000);
      }
    };
  }

  /** After a reconnect: everything may have changed. */
  private async resync() {
    await Promise.all([this.refreshSeats(), this.refreshCounts(), this.refreshAttention(), this.refreshOverview()]).catch(() => undefined);
  }

  private async flush(keys: Set<string>) {
    const mail = new Set<string>();
    let presenceChanged = false;
    for (const k of keys) {
      if (k.startsWith('m:')) mail.add(k.slice(2));
      else if (k.startsWith('p:')) presenceChanged = true;
    }
    if (presenceChanged && this.presence.size) {
      const updates = this.presence;
      this.presence = new Map();
      this.seats = this.seats.map((s) => {
        const p = updates.get(s.name);
        if (!p) return s;
        const { seat: _seat, ...presence } = p;
        return { ...s, presence };
      });
    }
    const jobs: Promise<unknown>[] = [];
    if (mail.size) {
      if (mail.size > ALL_COUNTS_OVER) jobs.push(this.refreshCounts());
      else jobs.push(...[...mail].map((s) => this.refreshCounts(s)));
    }
    const userSeat = this.me?.seat;
    if ((userSeat && mail.has(userSeat)) || presenceChanged) jobs.push(this.refreshAttention());
    await Promise.all(jobs).catch(() => undefined);
    this.scheduleOverview();
  }

  private scheduleOverview() {
    if (this.overviewTimer) return;
    const wait = Math.max(0, this.lastOverview + OVERVIEW_MIN_MS - Date.now());
    this.overviewTimer = setTimeout(() => {
      this.overviewTimer = null;
      void this.refreshOverview();
    }, wait);
  }

  async refreshSeats(): Promise<void> {
    this.seats = await get<Seat[]>('/v1/seats');
  }

  async refreshCounts(seat?: string): Promise<void> {
    const r = await get<CountsResponse>(`/v1/counts${qs({ seat })}`);
    const next = seat ? new Map(this.counts) : new Map<string, SeatCounts>();
    for (const c of r.seats) next.set(c.seat, c);
    this.counts = next;
    // Focus changes come as mailbox events: take them from the counts.
    const focus = new Map(r.seats.map((c) => [c.seat, c.focus_until]));
    if (this.seats.some((s) => focus.has(s.name) && focus.get(s.name) !== s.focus_until)) {
      this.seats = this.seats.map((s) => (focus.has(s.name) ? { ...s, focus_until: focus.get(s.name) } : s));
    }
  }

  async refreshAttention(): Promise<void> {
    const r = await get<{ items: AttentionItem[] }>('/v1/attention');
    this.attention = r.items;
  }

  async refreshOverview(): Promise<void> {
    this.lastOverview = Date.now();
    try {
      this.overview = await get<Overview>('/v1/overview');
    } catch {
      // Keep the last summary.
    }
  }

  async refreshDirectory(): Promise<void> {
    const [lists, links] = await Promise.all([get<ListInfo[]>('/v1/lists'), get<Link[]>('/v1/links')]);
    this.lists = lists;
    this.links = links;
  }

  /** Called after the user changed mail state (read, ack, sent). */
  async mailChanged(): Promise<void> {
    const s = this.me?.seat;
    await Promise.all([this.refreshAttention(), s ? this.refreshCounts(s) : Promise.resolve()]).catch(() => undefined);
    this.scheduleOverview();
  }

  async seatOp(name: string, op: 'sleep' | 'wake'): Promise<{ nodes: number }> {
    return post<{ nodes: number }>(`/v1/seats/${enc(name)}/${op}`);
  }
}

export const store = new Store();
