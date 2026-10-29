<script lang="ts">
  import { store } from '../lib/store.svelte';
  // Lists of the org, to say through which one mail arrived.
  const isList = (a: string) => store.lists.some((l) => l.name === a);
  import { t, ui } from '../lib/ui.svelte';
  import { go } from '../lib/router.svelte';
  import { get, qs } from '../lib/api';
  import { fromEntry, fromSent, newestFirst, type MailRow } from '../lib/mailrows';
  import { typing, moveIndex } from '../lib/keys';
  import { markAllRead, markReadMany, markUnreadMany } from '../lib/mailops';
  import {
    CONFIRM_ABOVE,
    headerState,
    markAllBefore,
    readable,
    selectedRows,
    toggleAll,
    toggled,
    unreadable,
    withRead,
  } from '../lib/selection';
  import type { ArchiveResponse, InboxEntry, InboxResponse } from '../lib/types';
  import MailList from '../components/MailList.svelte';
  import MessagePane from '../components/MessagePane.svelte';

  let { tab }: { tab: 'inbox' | 'sent' } = $props();

  const PAGE = 50;
  let entries = $state.raw<InboxEntry[]>([]);
  let sent = $state.raw<MailRow[]>([]);
  let sentNext = $state<number | undefined>(undefined);
  let more = $state(false);
  let error = $state<string | null>(null);
  let selected = $state<number | undefined>(undefined);
  let pane: MessagePane | undefined = $state();
  /** The selection was made by the person (not the automatic first row):
   * opening it marks it read. */
  let picked = $state(false);
  /** Rows ticked with their check boxes (Inbox tab only). */
  let checked = $state.raw<Set<number>>(new Set());
  let busy = $state(false);
  let actionError = $state<string | null>(null);
  $effect(() => {
    void tab;
    picked = false;
    checked = new Set();
    actionError = null;
  });

  const me = $derived(store.me?.seat);
  const seq = $derived(me ? (store.counts.get(me)?.seq ?? 0) : 0);
  const unreadCount = $derived(me ? (store.counts.get(me)?.unread ?? 0) : 0);
  const rows = $derived(tab === 'inbox' ? newestFirst(entries.map((e) => fromEntry(e, isList))) : sent);
  const index = $derived(rows.findIndex((r) => r.id === selected));
  const rowIds = $derived(rows.map((r) => r.id));
  const picks = $derived(selectedRows(checked, rowIds));
  const head = $derived(headerState(checked, rowIds));
  const canRead = $derived(readable(entries, picks).length > 0);
  const canUnread = $derived(unreadable(entries, picks).length > 0);

  let allBox: HTMLInputElement | undefined = $state();
  $effect(() => {
    if (allBox) allBox.indeterminate = head === 'some';
  });

  // (Re)load the newest page when the tab or the mailbox changes, keeping
  // older pages that were loaded.
  $effect(() => {
    void seq;
    const which = tab;
    error = null;
    if (which === 'inbox') {
      get<InboxResponse>(`/v1/inbox${qs({ before_seq: Number.MAX_SAFE_INTEGER, limit: PAGE })}`)
        .then((r) => {
          const ids = new Set(r.entries.map((e) => e.message.id));
          const oldest = r.entries.length ? r.entries[0].delivery.seq : Infinity;
          const kept = entries.filter((e) => !ids.has(e.message.id) && e.delivery.seq < oldest);
          if (!kept.length) more = r.entries.length === PAGE || !!r.truncated;
          entries = [...kept, ...r.entries];
        })
        .catch((e) => (error = String(e?.message ?? e)));
    } else {
      get<ArchiveResponse>(`/v1/sent${qs({ limit: PAGE, delivery: 'true' })}`)
        .then((r) => {
          sent = fromSent(r);
          sentNext = r.next_before_id;
        })
        .catch((e) => (error = String(e?.message ?? e)));
    }
  });

  $effect(() => {
    if (rows.length && index < 0) selected = rows[0].id;
  });

  async function older() {
    if (tab === 'inbox') {
      const min = Math.min(...entries.map((e) => e.delivery.seq));
      const r = await get<InboxResponse>(`/v1/inbox${qs({ before_seq: min, limit: PAGE })}`);
      more = r.entries.length === PAGE || !!r.truncated;
      entries = [...r.entries, ...entries];
    } else if (sentNext) {
      const r = await get<ArchiveResponse>(`/v1/sent${qs({ limit: PAGE, before_id: sentNext, delivery: 'true' })}`);
      sent = [...sent, ...fromSent(r)];
      sentNext = r.next_before_id;
    }
  }

  /** Fetch the loaded entries again (their read and ack state). */
  function refreshList() {
    get<InboxResponse>(`/v1/inbox${qs({ before_seq: Number.MAX_SAFE_INTEGER, limit: Math.max(PAGE, entries.length) })}`)
      .then((r) => (entries = r.entries))
      .catch(() => undefined);
  }

  function changed() {
    void store.mailChanged();
    // Reflect read/ack in the list right away.
    refreshList();
  }

  /** Run a bulk action: the rows were changed in place already; then the
   * list, the open message and (through mailops) the counts follow the
   * server. */
  async function bulk(f: () => Promise<unknown>) {
    busy = true;
    actionError = null;
    try {
      await f();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
      refreshList();
      pane?.refresh();
    }
  }

  function readSelected() {
    const ids = readable(entries, picks);
    if (!ids.length) return;
    entries = withRead(entries, ids, true, new Date().toISOString());
    void bulk(() => markReadMany(ids));
  }

  function unreadSelected() {
    const ids = unreadable(entries, picks);
    if (!ids.length) return;
    entries = withRead(entries, ids, false, '');
    void bulk(() => markUnreadMany(ids));
  }

  function readAll() {
    const before = markAllBefore(entries);
    if (before === undefined || unreadCount === 0) return;
    if (unreadCount > CONFIRM_ABOVE && !confirm(t('mail.markAllRead.confirm', { n: unreadCount }))) return;
    entries = withRead(
      entries,
      entries.map((e) => e.message.id),
      true,
      new Date().toISOString(),
    );
    void bulk(() => markAllRead(before));
  }

  function check(id: number, on?: boolean) {
    checked = toggled(checked, id, on);
  }

  function onKey(e: KeyboardEvent) {
    if (typing(e) || ui.helpOpen) return;
    const next = moveIndex(e.key, index, rows.length);
    if (next !== null) {
      selected = rows[next].id;
      picked = true;
      document.querySelector(`[data-msg="${selected}"]`)?.scrollIntoView({ block: 'nearest' });
      e.preventDefault();
      return;
    }
    const r = rows[index];
    if (!r) return;
    if (e.key === 'Enter' || e.key === 'o') {
      go(['thread', r.thread_id], { m: r.id });
      e.preventDefault();
    } else if (e.key === 'r') {
      pane?.doReply(false);
      e.preventDefault();
    } else if (e.key === 'e') {
      pane?.doMarkRead();
      e.preventDefault();
    } else if (e.key === 'u') {
      pane?.doMarkUnread();
      e.preventDefault();
    } else if (e.key === 'a') {
      pane?.doAck();
      e.preventDefault();
    } else if (e.key === 'x' && tab === 'inbox') {
      check(r.id);
      e.preventDefault();
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="inbox">
  <section class="listpane">
    <div class="tabs" role="tablist">
      <button role="tab" aria-selected={tab === 'inbox'} onclick={() => go(['inbox'])}>
        {t('mail.inbox')}{#if me && store.counts.get(me)?.unread}<span class="count hot">{store.counts.get(me)?.unread}</span>{/if}
      </button>
      <button role="tab" aria-selected={tab === 'sent'} onclick={() => go(['inbox'], { tab: 'sent' })}>{t('mail.sent')}</button>
    </div>
    {#if tab === 'inbox'}
      <div class="toolbar">
        <label class="pickall">
          <input
            type="checkbox"
            data-testid="check-all"
            bind:this={allBox}
            checked={head === 'all'}
            disabled={rows.length === 0}
            aria-label={t('mail.selectAll')}
            title={t('mail.selectAll')}
            onchange={() => (checked = toggleAll(checked, rowIds))}
          />
        </label>
        <span class="spacer"></span>
        <button class="small" data-testid="mark-all-read" title={t('mail.markAllRead.long')} disabled={busy || unreadCount === 0 || entries.length === 0} onclick={readAll}>
          {t('mail.markAllRead')}
        </button>
      </div>
      {#if picks.length}
        <div class="selbar" role="region" aria-label={t('mail.selection')} data-testid="selection-bar">
          <span class="selcount" data-testid="selected-count">{t('mail.selectedCount', { n: picks.length })}</span>
          <button class="small" disabled={busy || !canRead} onclick={readSelected}>{t('mail.markRead')}</button>
          <button class="small" disabled={busy || !canUnread} onclick={unreadSelected}>{t('mail.markUnread')}</button>
          <button class="small ghost" onclick={() => (checked = new Set())}>{t('mail.clearSelection')}</button>
        </div>
      {/if}
      {#if actionError}<p class="error pad" role="alert">{actionError}</p>{/if}
    {/if}
    <div class="scroll">
      {#if error}
        <p class="error pad">{error}</p>
      {:else if rows.length === 0}
        <div class="empty">{t('mail.empty')}</div>
      {:else}
        <MailList
          {rows}
          {selected}
          showTo={tab === 'sent'}
          label={tab === 'sent' ? t('mail.sent') : t('mail.inbox')}
          checked={tab === 'inbox' ? checked : undefined}
          oncheck={(r, on) => check(r.id, on)}
          onselect={(r) => {
            selected = r.id;
            picked = true;
          }}
          onopen={(r) => go(['thread', r.thread_id], { m: r.id })}
        />
        {#if (tab === 'inbox' && more) || (tab === 'sent' && sentNext)}
          <div class="more"><button class="small" onclick={older}>{t('mail.older')}</button></div>
        {/if}
      {/if}
    </div>
  </section>
  <section class="reader">
    {#if selected !== undefined}
      {#key selected}
        <MessagePane bind:this={pane} id={selected} onchange={changed} markOnOpen={picked && tab === 'inbox'} />
      {/key}
    {:else}
      <div class="empty">{t('mail.select')}</div>
    {/if}
  </section>
</div>

<style>
  .inbox {
    display: grid;
    grid-template-columns: minmax(380px, 46%) 1fr;
    height: 100%;
    min-height: 0;
  }
  .listpane {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-right: 1px solid var(--border);
    background: var(--surface);
  }
  .tabs {
    padding: 6px 10px 0;
  }
  .tabs .count {
    margin-left: 5px;
  }
  .toolbar,
  .selbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 12px 4px 0;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .selbar {
    padding-left: 12px;
    background: var(--accent-weak);
  }
  .selcount {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .pickall {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    min-height: 28px;
    cursor: pointer;
  }
  .pickall input {
    width: 16px;
    height: 16px;
    margin: 0;
    cursor: pointer;
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
  }
  .more {
    padding: 10px;
    text-align: center;
  }
  .reader {
    overflow-y: auto;
    padding: 12px 16px;
    min-width: 0;
  }
  .pad {
    padding: 12px;
  }
  @media (max-width: 1000px) {
    /* One column: the list (with its own scroll) above the message; the
     * page scrolls, so that the list keeps a usable height under the
     * toolbar and the selection bar. */
    .inbox {
      grid-template-columns: 1fr;
      height: auto;
    }
    .listpane {
      border-right: none;
      border-bottom: 1px solid var(--border);
    }
    .scroll {
      flex: none;
      max-height: 60vh;
    }
  }
  @media (max-width: 720px) {
    .pickall {
      width: 40px;
      min-height: 36px;
    }
    .pickall input {
      width: 20px;
      height: 20px;
    }
  }
</style>
