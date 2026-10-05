<script lang="ts">
  import { visible } from '../lib/visible';
  import type { Row } from '../lib/tree';
  import type { Seat, SeatCounts, SeatState } from '../lib/types';
  import { SEAT_STATES } from '../lib/types';
  import { KNOWN_ATTENTION, focusUntil, seatStatus } from '../lib/presence';
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

  const status = $derived(seatStatus(seat, now));
  const seatState: SeatState = $derived(status.state);
  const p = $derived(status.presence);
  const age = $derived(status.age);
  const isUser = $derived(seat.kind === 'user');
  const focus = $derived(focusUntil(seat, now));
  const hm = $derived([seat.harness ?? p?.harness, seat.model].filter(Boolean).join(' · '));
  const tag = $derived(status.tag);
  const attnLabel = $derived.by(() => {
    if (!tag) return null;
    return KNOWN_ATTENTION.includes(tag) ? t(`attn.${tag}` as 'attn.login') : tag;
  });
  const attnTitle = $derived(
    p?.attention_hint ?? (p?.turn_failed ? t('flag.turnFailed.long', { reason: p.turn_failed.reason, n: p.turn_failed.count }) : t('flag.attention')),
  );
  const failing = $derived(status.failing);
  const starting = $derived(status.starting);
</script>

<div
  class="trow"
  class:selected
  class:context={row.context}
  class:user={isUser}
  class:failing
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
    <span class="mono seatname" title={visible(seat.display_name ?? seat.name)}>{seat.name}</span>
    {#if isUser}<span class="chip neutral">{t('org.you')}</span>{/if}
    {#if seat.open_to?.length}<span class="chip open" data-testid="open-chip" title={t('open.to', { list: seat.open_to.map((a) => (a === 'linked' ? t('open.linked') : a)).join(', ') })}>{t('open.chip')}</span>{/if}
    {#if seat.description}<span class="desc faint" title={visible(seat.description)}>{visible(seat.description)}</span>{/if}
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
  <div class="hm faint">{isUser ? '' : visible(hm)}</div>
  <div class="state">{#if !isUser}<StateBadge state={seatState} {age} {starting} {failing} />{/if}</div>
  <div class="flags">
    {#if attnLabel}<span class="chip attn" title={visible(attnTitle)}>⚠ {attnLabel}</span>{/if}
    {#if p?.turn_failed && tag !== 'turn-failed'}<span class="chip attn" title={visible(t('flag.turnFailed.long', { reason: p.turn_failed.reason, n: p.turn_failed.count }))}>⚠ {t('attn.turn-failed')}</span>{/if}
    {#if p?.wake_failed && tag !== 'wake-failed'}<span class="chip attn" title={t('flag.wakeFailed.long')}>{t('flag.wakeFailed')}</span>{/if}
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
  .trow.failing {
    box-shadow: inset 3px 0 0 var(--danger);
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
    flex: none;
    max-width: 70%;
    font-size: 12.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 550;
  }
  .desc {
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .chip.open {
    color: var(--ok);
    background: var(--ok-weak);
    flex: none;
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
    .desc {
      display: none;
    }
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
