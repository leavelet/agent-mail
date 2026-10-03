<script lang="ts">
  import type { SeatState } from '../lib/types';
  import { t, ui } from '../lib/ui.svelte';
  import { duration } from '../lib/time';

  let { state, age, compact = false }: { state: SeatState; age?: number; compact?: boolean } = $props();
</script>

<span class="badge st-{state}" title={t(`state.${state}.long`)} data-state={state}>
  <span class="dot" aria-hidden="true"></span>{t(`state.${state}`)}{#if age !== undefined && !compact}<span class="age">{duration(age, ui.lang)}</span>{/if}
</span>

<style>
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 0 7px 0 6px;
    border-radius: 10px;
    font-size: 11.5px;
    line-height: 19px;
    white-space: nowrap;
    color: var(--c);
    background: var(--bgc);
    font-weight: 550;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--c);
    flex: none;
  }
  .age {
    font-weight: 400;
    opacity: 0.8;
    font-variant-numeric: tabular-nums;
  }
  .st-busy {
    --c: var(--st-busy);
    --bgc: var(--st-busy-bg);
  }
  .st-busy .dot {
    animation: pulse 1.6s ease-in-out infinite;
  }
  .st-idle {
    --c: var(--st-idle);
    --bgc: var(--st-idle-bg);
  }
  .st-awaiting {
    --c: var(--st-awaiting);
    --bgc: var(--st-awaiting-bg);
  }
  .st-dormant {
    --c: var(--st-dormant);
    --bgc: var(--st-dormant-bg);
  }
  .st-offline {
    --c: var(--st-offline);
    --bgc: var(--st-offline-bg);
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .st-busy .dot {
      animation: none;
    }
  }
</style>
