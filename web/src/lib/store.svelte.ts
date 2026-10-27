// Shared live state: who is signed in, the seats with their presence and
// mailbox counters, the "needs you" queue and the org summary. Live updates
// come from the server's event stream and are merged into about one update
// per second.

import { ApiError, get, post, del, qs, enc, request, setUnauthorizedHandler } from './api';
import { apiUrl } from './base';
import { Coalescer } from './coalesce';
import { announce, itemKey, newItems } from './notify';
import { go } from './router.svelte';
import { t } from './ui.svelte';
import { activeOrgs } from './openings';
import type {
  AttentionItem,
  Card,
  Grant,
  OrgInfo,
  PeerOrg,
  Team,
  CountsResponse,
  DirectoryEvent,
  Link,
  ListInfo,
  MailboxEvent,
  NodesEvent,
  Overview,
  PresenceEvent,
  Seat,
  SeatCounts,
  ServerInfo,
  TaskBusEvent,
  DocBusEvent,
  WhoAmI,
} from './types';

export type AuthState = 'checking' | 'out' | 'in' | 'operator';

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
  teams = $state.raw<Team[]>([]);
  grants = $state.raw<Grant[]>([]);
  /** Per org with an active link: the cards it opened to this org (loaded on demand). */
  peerCards = $state.raw<Map<string, Card[]>>(new Map());
  /** The one-time notice of the org for the user, until dismissed. */
  orgNotice = $state<string | null>(null);
  /** The event stream is open. */
  live = $state(false);
  /** Node event streams open for the org (null: not known yet). */
  nodesConnected = $state<number | null>(null);
  /** What the server says about itself (public mode, retention). */
  info = $state.raw<ServerInfo | null>(null);
  loadError = $state<string | null>(null);
  /** Counts the rounds of `task` events (and stream openings): views that
   * show tasks reload when it changes. */
  taskSeq = $state(0);
  /** The tasks changed in the last round; null when anything may have
   * changed (the stream was opened again). */
  taskChanged = $state.raw<Set<number> | null>(null);
  /** Counts the rounds of `doc` events (and stream openings): views that
   * show documents reload when it changes. */
  docSeq = $state(0);
  /** The documents changed in the last round; null when anything may have
   * changed. */
  docChanged = $state.raw<Set<number> | null>(null);

  seatMap = $derived(new Map(this.seats.map((s) => [s.name, s])));

  private es: EventSource | null = null;
  private presence = new Map<string, PresenceEvent>();
  private coalescer = new Coalescer<string>(FLUSH_MS, (keys) => void this.flush(keys));
  private lastOverview = 0;
  private overviewTimer: ReturnType<typeof setTimeout> | null = null;
  private timers: ReturnType<typeof setInterval>[] = [];
  /** Keys of the attention items seen so far (null: not loaded yet). */
  private seenAttention: Set<string> | null = null;

  constructor() {
    setUnauthorizedHandler(() => this.signedOut());
  }

  async loadInfo(): Promise<void> {
    try {
      this.info = await get<ServerInfo>('/v1/info');
    } catch {
      // An older server: no public mode.
    }
  }

  async check(): Promise<void> {
    void this.loadInfo();
    try {
      const me = await get<WhoAmI>('/v1/whoami');
      if (me.scope === 'operator') {
        this.me = me;
        this.auth = 'operator';
        return;
      }
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
    if (me.scope === 'operator') {
      this.auth = 'operator';
      return;
    }
    this.auth = 'in';
    await this.start();
  }

  /** Replace the signed-in user token; the session moves to the new one. */
  async rotateToken(): Promise<string> {
    const r = await post<{ token: string }>('/v1/tokens/rotate', {});
    return r.token;
  }

  /** Delete the org with all its data; the session ends with it. */
  async deleteOrg(confirm: string): Promise<void> {
    await request<void>('DELETE', '/v1/org', { confirm });
    this.signedOut();
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
    this.seenAttention = null;
  }

  private async start() {
    this.loadError = null;
    void this.loadNotice();
    await Promise.all([this.refreshSeats(), this.refreshCounts(), this.refreshAttention(), this.refreshOverview(), this.refreshDirectory(), this.refreshOpenings()]).catch(
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
    const es = new EventSource(apiUrl('/v1/events'));
    this.es = es;
    es.onopen = () => {
      this.live = true;
      // Presence events are not replayed: a change between the first load
      // of the seats and this (re)connection would be missed, so load the
      // seats again once the stream is open.
      this.coalescer.add('d:seats');
      // Task events are not replayed either: views that show tasks reload.
      this.coalescer.add('t:*');
      this.coalescer.add('x:*');
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
    // A seat or list was created, changed or deleted: reload the directory
    // (merged with the other updates of the same second).
    es.addEventListener('directory', (e) => {
      try {
        const d = JSON.parse((e as MessageEvent).data) as DirectoryEvent;
        // Teams and grants change seats too (team, contact, open to).
        if (d.kind === 'list') this.coalescer.add('d:lists');
        else this.coalescer.add('d:seats');
        if (d.kind === 'team' || d.kind === 'grant') this.coalescer.add('d:openings');
      } catch {
        // Ignore malformed events.
      }
    });
    // A task of the org changed: views that show it reload (merged with
    // the other updates of the same second).
    es.addEventListener('task', (e) => {
      try {
        const ev = JSON.parse((e as MessageEvent).data) as TaskBusEvent;
        if (typeof ev.num === 'number') this.coalescer.add(`t:${ev.num}`);
      } catch {
        // Ignore malformed events.
      }
    });
    // A document of the org changed: views that show it reload.
    es.addEventListener('doc', (e) => {
      try {
        const ev = JSON.parse((e as MessageEvent).data) as DocBusEvent;
        if (typeof ev.num === 'number') this.coalescer.add(`x:${ev.num}`);
      } catch {
        // Ignore malformed events.
      }
    });
    // A node connected or went away: sleep and wake work only with one.
    es.addEventListener('nodes', (e) => {
      try {
        const n = JSON.parse((e as MessageEvent).data) as NodesEvent;
        this.setNodesConnected(n.connected);
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

  /** Apply the number of connected nodes to every seat's presence. */
  setNodesConnected(n: number) {
    this.nodesConnected = n;
    const connected = n > 0;
    if (this.seats.some((s) => s.presence && s.presence.node_connected !== connected)) {
      this.seats = this.seats.map((s) => (s.presence && s.presence.node_connected !== connected ? { ...s, presence: { ...s.presence, node_connected: connected } } : s));
    }
  }

  private async flush(keys: Set<string>) {
    const mail = new Set<string>();
    let presenceChanged = false;
    const jobs: Promise<unknown>[] = [];
    let tasks: Set<number> | null = new Set();
    let anyTask = false;
    let docs: Set<number> | null = new Set();
    let anyDoc = false;
    for (const k of keys) {
      if (k.startsWith('m:')) mail.add(k.slice(2));
      else if (k.startsWith('p:')) presenceChanged = true;
      else if (k.startsWith('t:')) {
        anyTask = true;
        const n = Number(k.slice(2));
        if (k === 't:*' || !Number.isFinite(n)) tasks = null;
        else tasks?.add(n);
      } else if (k.startsWith('x:')) {
        anyDoc = true;
        const n = Number(k.slice(2));
        if (k === 'x:*' || !Number.isFinite(n)) docs = null;
        else docs?.add(n);
      }
    }
    if (anyDoc) {
      this.docChanged = docs;
      this.docSeq++;
    }
    if (anyTask) {
      this.taskChanged = tasks;
      this.taskSeq++;
    }
    if (keys.has('d:seats')) {
      // The new list replaces the seats; presence updates of this round are
      // in it already or follow.
      jobs.push(this.refreshSeats().then(() => this.refreshCounts()));
    }
    if (keys.has('d:lists')) jobs.push(this.refreshDirectory());
    if (keys.has('d:openings')) jobs.push(this.refreshOpenings());
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
    const seats = await get<Seat[]>('/v1/seats');
    // The node count from the event stream is newer than a list fetched
    // while a node (dis)connected.
    const n = this.nodesConnected;
    this.seats =
      n === null
        ? seats
        : seats.map((s) => (s.presence && s.presence.node_connected !== n > 0 ? { ...s, presence: { ...s.presence, node_connected: n > 0 } } : s));
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
    const fresh = newItems(this.seenAttention, r.items);
    this.seenAttention = new Set(r.items.map(itemKey));
    announce(fresh, t, (i) => {
      if (i.message) go(['thread', i.message.thread_id], { m: i.message.id });
      else go(['needs']);
    });
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

  /** Teams and grants of the org. */
  async refreshOpenings(): Promise<void> {
    const [teams, grants] = await Promise.all([get<Team[]>('/v1/teams'), get<Grant[]>('/v1/grants')]);
    this.teams = teams;
    this.grants = grants;
  }

  /** After a change of grants, teams or links: everything they show. */
  async openingsChanged(): Promise<void> {
    this.peerLoaded = '';
    await Promise.all([this.refreshOpenings(), this.refreshSeats(), this.refreshDirectory()]).catch(() => undefined);
  }

  private peerLoaded = '';
  /** Load the cards each org with an active link opened to this org (for
   * address completion); again only when the active links changed. */
  async loadPeers(): Promise<void> {
    const orgs = activeOrgs(this.links);
    const key = orgs.join(',');
    if (key === this.peerLoaded) return;
    this.peerLoaded = key;
    const next = new Map<string, Card[]>();
    await Promise.all(
      orgs.map((o) =>
        get<PeerOrg>(`/v1/orgs/${enc(o)}`)
          .then((p) => next.set(o, p.open_to_you))
          .catch(() => undefined),
      ),
    );
    this.peerCards = next;
  }

  async loadNotice(): Promise<void> {
    try {
      this.orgNotice = (await get<OrgInfo>('/v1/org')).notice ?? null;
    } catch {
      // No notice.
    }
  }

  async dismissNotice(): Promise<void> {
    this.orgNotice = null;
    await del('/v1/org/notice').catch(() => undefined);
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
