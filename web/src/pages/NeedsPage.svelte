<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { href, go } from '../lib/router.svelte';
  import { attentionKey, isUrgent } from '../lib/attention';
  import { duration } from '../lib/time';
  import { typing, moveIndex } from '../lib/keys';
  import { parseTime } from '../lib/presence';
  import type { AttentionItem } from '../lib/types';
  import MessagePane from '../components/MessagePane.svelte';
  import StateBadge from '../components/StateBadge.svelte';

  let selectedKey = $state<string | null>(null);
  let pane: MessagePane | undefined = $state();
  const items = $derived(store.attention);
  const index = $derived(items.findIndex((it) => attentionKey(it) === selectedKey));
  const current = $derived<AttentionItem | undefined>(index >= 0 ? items[index] : undefined);

  // Select the first item initially (and when the selection disappears).
  $effect(() => {
    if (items.length && index < 0) selectedKey = attentionKey(items[0]);
  });

  function waited(it: AttentionItem): string {
    const s = parseTime(it.since);
    return Number.isNaN(s) ? '' : duration(ui.now - s, ui.lang);
  }

  function attnLabel(a: string | undefined): string {
    if (!a) return '';
    const k = `attn.${a}`;
    return ['attn.trust-prompt', 'attn.login', 'attn.permission', 'attn.failed', 'attn.wake-failed', 'attn.no-activity'].includes(k) ? t(k as 'attn.login') : a;
  }

  function open(it: AttentionItem) {
    if (it.kind === 'seat' && it.seat) go(['org', it.seat]);
    else if (it.message) go(['thread', it.message.thread_id], { m: it.message.id });
  }

  function onKey(e: KeyboardEvent) {
    if (typing(e) || ui.helpOpen) return;
    const next = moveIndex(e.key, index, items.length);
    if (next !== null) {
      selectedKey = attentionKey(items[next]);
      document.querySelector(`[data-key="${CSS.escape(selectedKey)}"]`)?.scrollIntoView({ block: 'nearest' });
      e.preventDefault();
      return;
    }
    if (!current) return;
    if (e.key === 'Enter' || e.key === 'o') {
      open(current);
      e.preventDefault();
    } else if (current.kind !== 'seat' && pane) {
      if (e.key === 'r') {
        pane.doReply(false);
        e.preventDefault();
      } else if (e.key === 'e') {
        pane.doMarkRead();
        e.preventDefault();
      } else if (e.key === 'a') {
        pane.doAck();
        e.preventDefault();
      }
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="needs">
  <section class="queue" aria-label={t('needs.title')}>
    <div class="qhead">
      <h1>{t('needs.title')} <span class="count {items.length ? 'alert' : ''}">{items.length}</span></h1>
      <p class="muted intro">{t('needs.intro')}</p>
    </div>
    {#if items.length === 0}
      <div class="empty done">✓ {t('needs.empty')}</div>
    {:else}
      <div class="items" role="listbox" aria-label={t('needs.title')}>
        {#each items as it (attentionKey(it))}
          {@const key = attentionKey(it)}
          {@const seat = it.seat ? store.seatMap.get(it.seat) : undefined}
          <div
            class="item k-{it.kind}"
            class:sel={key === selectedKey}
            class:urgent={isUrgent(it)}
            role="option"
            aria-selected={key === selectedKey}
            tabindex="-1"
            data-key={key}
            data-kind={it.kind}
            onclick={() => (selectedKey = key)}
            ondblclick={() => open(it)}
            onkeydown={(e) => {
              if (e.key === 'Enter') open(it);
            }}
          >
            <span class="kind" title={t(`needs.kind.${it.kind}.long`)}>{t(`needs.kind.${it.kind}`)}</span>
            <div class="main">
              <div class="l1">
                {#if it.seat}<span class="mono who">{it.seat}</span>{/if}
                {#if it.kind === 'seat'}
                  {#if it.state}<StateBadge state={it.state} compact />{/if}
                  <span class="chip attn">⚠ {it.attention ? attnLabel(it.attention) : t('flag.wakeFailed')}</span>
                {:else if it.message}
                  {#if it.message.priority === 'urgent'}<span class="chip urgent">{t('mail.urgent')}</span>{/if}
                  {#if it.message.source === 'external'}<span class="chip neutral">{t('mail.source.external')}</span>{/if}
                  <span class="subj">{it.message.subject || t('mail.noSubject')}</span>
                {/if}
              </div>
              {#if it.kind === 'seat' && it.hint}<div class="l2 muted">{it.hint}</div>{/if}
              {#if it.kind !== 'seat' && seat}<div class="l2 faint">{seat.role?.split('\n', 1)[0] ?? ''}</div>{/if}
            </div>
            <span class="age faint" title={it.since}>{t('needs.waiting', { d: waited(it) })}</span>
          </div>
        {/each}
      </div>
    {/if}
    <p class="faint rule">{t('needs.rule')}</p>
  </section>
  <section class="reader">
    {#if current?.kind === 'seat' && current.seat}
      {@const seat = store.seatMap.get(current.seat)}
      <div class="seatcard card">
        <div class="row"><h2 class="mono">{current.seat}</h2>{#if current.state}<StateBadge state={current.state} />{/if}</div>
        <p class="error"><strong>⚠ {current.attention ? attnLabel(current.attention) : t('flag.wakeFailed')}</strong></p>
        {#if current.hint}<p>{current.hint}</p>{/if}
        {#if seat?.presence?.terminal_url}
          <p><a href={seat.presence.terminal_url} target="_blank" rel="noopener noreferrer">{t('seat.openTerminal')} ↗</a></p>
        {:else}
          <p class="muted">{t('seat.attachCmd')} <code>amail attach {current.seat}</code></p>
        {/if}
        <p><a href={href(['org', current.seat])}>{t('nav.org')}: {current.seat} →</a></p>
      </div>
    {:else if current?.message}
      {#key current.message.id}
        <MessagePane bind:this={pane} id={current.message.id} onchange={() => store.mailChanged()} />
      {/key}
    {:else}
      <div class="empty">{t('mail.select')}</div>
    {/if}
  </section>
</div>

<style>
  .needs {
    display: grid;
    grid-template-columns: minmax(360px, 44%) 1fr;
    height: 100%;
    min-height: 0;
  }
  .queue {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-right: 1px solid var(--border);
    background: var(--surface);
  }
  .qhead {
    padding: 12px 14px 8px;
    border-bottom: 1px solid var(--border);
  }
  .intro {
    margin: 4px 0 0;
    font-size: 12px;
  }
  .items {
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }
  .item {
    display: grid;
    grid-template-columns: 62px 1fr auto;
    gap: 10px;
    align-items: start;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    cursor: pointer;
  }
  .item:hover {
    background: var(--surface-2);
  }
  .item.sel {
    background: var(--accent-weak);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .item.urgent {
    box-shadow: inset 3px 0 0 var(--danger);
  }
  .item.urgent.sel {
    box-shadow: inset 3px 0 0 var(--danger);
  }
  .kind {
    font-size: 10.5px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    text-align: center;
    border-radius: 4px;
    padding: 1px 0;
    margin-top: 1px;
  }
  .k-ack .kind {
    color: var(--warn);
    background: var(--warn-weak);
  }
  .k-reply .kind {
    color: var(--accent);
    background: var(--accent-weak);
  }
  .k-seat .kind {
    color: var(--danger);
    background: var(--danger-weak);
  }
  .main {
    min-width: 0;
  }
  .l1 {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .who {
    font-weight: 650;
    font-size: 12.5px;
    white-space: nowrap;
    flex: none;
  }
  .subj {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .l2 {
    font-size: 12px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    margin-top: 1px;
  }
  .age {
    font-size: 12px;
    white-space: nowrap;
  }
  .rule {
    font-size: 11.5px;
    padding: 8px 14px;
    margin: 0;
    border-top: 1px solid var(--border);
  }
  .done {
    color: var(--ok);
    font-size: 14px;
  }
  .reader {
    overflow-y: auto;
    padding: 12px 16px;
    min-width: 0;
  }
  .seatcard {
    padding: 12px 16px;
  }
  .seatcard p {
    margin: 8px 0 0;
  }
  @media (max-width: 1000px) {
    .needs {
      grid-template-columns: 1fr;
    }
    .reader {
      border-top: 1px solid var(--border);
    }
  }
</style>
