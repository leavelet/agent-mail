<script lang="ts">
  import { store } from '../lib/store.svelte';
  // Lists of the org, to say through which one mail arrived.
  const isList = (a: string) => store.lists.some((l) => l.name === a);
  import { t, ui } from '../lib/ui.svelte';
  import { go } from '../lib/router.svelte';
  import { get, qs } from '../lib/api';
  import { fromEntry, fromHeader, newestFirst, type MailRow } from '../lib/mailrows';
  import { typing, moveIndex } from '../lib/keys';
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

  const me = $derived(store.me?.seat);
  const seq = $derived(me ? (store.counts.get(me)?.seq ?? 0) : 0);
  const rows = $derived(tab === 'inbox' ? newestFirst(entries.map((e) => fromEntry(e, isList))) : sent);
  const index = $derived(rows.findIndex((r) => r.id === selected));

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
      get<ArchiveResponse>(`/v1/sent${qs({ limit: PAGE })}`)
        .then((r) => {
          sent = r.messages.map(fromHeader);
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
      const r = await get<ArchiveResponse>(`/v1/sent${qs({ limit: PAGE, before_id: sentNext })}`);
      sent = [...sent, ...r.messages.map(fromHeader)];
      sentNext = r.next_before_id;
    }
  }

  function changed() {
    void store.mailChanged();
    // Reflect read/ack in the list right away.
    get<InboxResponse>(`/v1/inbox${qs({ before_seq: Number.MAX_SAFE_INTEGER, limit: Math.max(PAGE, entries.length) })}`)
      .then((r) => (entries = r.entries))
      .catch(() => undefined);
  }

  function onKey(e: KeyboardEvent) {
    if (typing(e) || ui.helpOpen) return;
    const next = moveIndex(e.key, index, rows.length);
    if (next !== null) {
      selected = rows[next].id;
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
    } else if (e.key === 'a') {
      pane?.doAck();
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
          onselect={(r) => (selected = r.id)}
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
        <MessagePane bind:this={pane} id={selected} onchange={changed} />
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
    .inbox {
      grid-template-columns: 1fr;
    }
  }
</style>
