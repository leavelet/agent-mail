<script lang="ts">
  import type { Row } from '../lib/tree';
  import type { Seat, SeatCounts, SeatState } from '../lib/types';
  import { SEAT_STATES } from '../lib/types';
  import { effectiveState, focusUntil, freshPresence, stateAge } from '../lib/presence';
  import { shortTime } from '../lib/time';
  import { t, ui } from '../lib/ui.svelte';
  import StateBadge from './StateBadge.svelte';

  let {
    row,
    seat,
    counts,
    waiting = false,
    below,
    size = 0,
    selected = false,
    now,
    ontoggle,
    onselect,
  }: {
    row: Row;
    seat: Seat;
    counts?: SeatCounts;
    waiting?: boolean;
    below?: Record<string, number>;
    size?: number;
    selected?: boolean;
    now: number;
    ontoggle?: (name: string) => void;
    onselect?: (name: string) => void;
  } = $props();

  const seatState: SeatState = $derived(effectiveState(seat, now));
  const p = $derived(freshPresence(seat.presence, now));
  const age = $derived(stateAge(seat, now));
  const isUser = $derived(seat.kind === 'user');
  const focus = $derived(focusUntil(seat, now));
  const hm = $derived([seat.harness ?? p?.harness, seat.model].filter(Boolean).join(' · '));
  const attnLabel = $derived.by(() => {
    const a = p?.attention;
    if (!a) return null;
    const key = `attn.${a}`;
    return ['attn.trust-prompt', 'attn.login', 'attn.permission', 'attn.failed', 'attn.wake-failed', 'attn.no-activity'].includes(key)
      ? t(key as 'attn.login')
      : a;
  });
</script>

<div
  class="trow"
  class:selected
  class:context={row.context}
  class:user={isUser}
  role="treeitem"
  aria-level={row.depth + 1}
  aria-expanded={row.hasChildren ? row.expanded : undefined}
  aria-selected={selected}
  data-seat={seat.name}
  tabindex="-1"
  onclick={() => onselect?.(seat.name)}
  onkeydown={(e) => {
    if (e.key === 'Enter') onselect?.(seat.name);
  }}
>
  <div class="name" style="padding-left: {row.depth * 16 + 4}px">
    {#if row.hasChildren}
      <button
        class="caret"
        aria-label={row.expanded ? t('org.collapse', { name: seat.name }) : t('org.expand', { name: seat.name })}
        onclick={(e) => {
          e.stopPropagation();
          ontoggle?.(seat.name);
        }}>{row.expanded ? '▾' : '▸'}</button
      >
    {:else}
      <span class="caret-space"></span>
    {/if}
    <span class="mono seatname" title={seat.display_name ?? seat.name}>{seat.name}</span>
    {#if isUser}<span class="chip neutral">{t('org.you')}</span>{/if}
    {#if row.hasChildren && !row.expanded && below}
      <span class="below" title={t('org.subtree', { n: size })}>
        {#each SEAT_STATES as s}
          {#if below[s]}<span class="b st-{s}" title={t(`state.${s}.long`)}>{below[s]}</span>{/if}
        {/each}
        {#if below.needs}<span class="b alert" title={t('flag.attention')}>!{below.needs}</span>{/if}
        {#if below.waiting}<span class="b you" title={t('flag.waitingYou.long')}>✉{below.waiting}</span>{/if}
      </span>
    {/if}
  </div>
  <div class="hm faint">{isUser ? '' : hm}</div>
  <div class="state">{#if !isUser}<StateBadge state={seatState} {age} />{/if}</div>
  <div class="flags">
    {#if attnLabel}<span class="chip attn" title={p?.attention_hint ?? t('flag.attention')}>⚠ {attnLabel}</span>{/if}
    {#if p?.wake_failed && p?.attention !== 'wake-failed'}<span class="chip attn" title={t('flag.wakeFailed.long')}>{t('flag.wakeFailed')}</span>{/if}
    {#if focus}<span class="chip focus" title={t('flag.focus.long')}>◐ {t('flag.focus', { time: shortTime(focus, now, ui.lang) })}</span>{/if}
    {#if waiting}<span class="chip you" title={t('flag.waitingYou.long')}>✉ {t('flag.waitingYou')}</span>{/if}
  </div>
  <div class="mail">
    {#if counts?.unread}<span class="count hot" title={t('flag.unread', { n: counts.unread })}>{counts.unread}</span>{/if}
    {#if counts?.pending && !isUser}<span class="count" title={t('flag.pending', { n: counts.pending })}>+{counts.pending}</span>{/if}
  </div>
</div>

<style>
  .trow {
    display: grid;
    grid-template-columns: minmax(180px, 1fr) minmax(0, 170px) 128px minmax(0, 220px) 72px;
    align-items: center;
    gap: 8px;
    height: var(--row-h);
    padding-right: 10px;
    border-bottom: 1px solid var(--border);
    cursor: pointer;
    background: var(--surface);
  }
  .trow:hover {
    background: var(--surface-2);
  }
  .trow.selected {
    background: var(--accent-weak);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .trow.context {
    opacity: 0.55;
  }
  .name {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    overflow: hidden;
  }
  .seatname {
    font-size: 12.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 550;
  }
  .user .seatname {
    color: var(--accent);
  }
  .caret {
    width: 18px;
    height: 18px;
    padding: 0;
    border: none;
    background: transparent;
    color: var(--muted);
    line-height: 18px;
    flex: none;
  }
  .caret-space {
    width: 18px;
    flex: none;
  }
  .hm {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
  }
  .flags {
    display: flex;
    gap: 4px;
    overflow: hidden;
    min-width: 0;
  }
  .chip.you {
    color: var(--accent);
    background: var(--accent-weak);
    font-weight: 600;
  }
  .mail {
    display: flex;
    gap: 4px;
    justify-content: flex-end;
  }
  .below {
    display: inline-flex;
    gap: 3px;
    margin-left: 4px;
  }
  .b {
    font-size: 10.5px;
    line-height: 16px;
    padding: 0 5px;
    border-radius: 8px;
    font-variant-numeric: tabular-nums;
    color: var(--c);
    background: var(--bgc);
    font-weight: 600;
  }
  .b.st-busy {
    --c: var(--st-busy);
    --bgc: var(--st-busy-bg);
  }
  .b.st-idle {
    --c: var(--st-idle);
    --bgc: var(--st-idle-bg);
  }
  .b.st-awaiting {
    --c: var(--st-awaiting);
    --bgc: var(--st-awaiting-bg);
  }
  .b.st-dormant {
    --c: var(--st-dormant);
    --bgc: var(--st-dormant-bg);
  }
  .b.st-offline {
    --c: var(--st-offline);
    --bgc: var(--st-offline-bg);
  }
  .b.alert {
    --c: var(--danger);
    --bgc: var(--danger-weak);
  }
  .b.you {
    --c: var(--accent);
    --bgc: var(--accent-weak);
  }
  @container tree (max-width: 720px) {
    .trow {
      grid-template-columns: minmax(140px, 1fr) 118px minmax(0, 150px) 60px;
    }
    .hm {
      display: none;
    }
  }
  @container tree (max-width: 480px) {
    .trow {
      grid-template-columns: minmax(120px, 1fr) 100px 52px;
    }
    .flags {
      display: none;
    }
  }
</style>
