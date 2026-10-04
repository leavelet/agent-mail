<script lang="ts">
  import type { SeatState } from '../lib/types';
  import { t, ui } from '../lib/ui.svelte';
  import { duration } from '../lib/time';

  /** `starting`: the node reports the seat offline while its session waits
   * at a prompt for a person; it is shown as starting. `failing`: its turns
   * fail (shown in red). `long`: say both in words (where there is room). */
  let {
    state,
    age,
    compact = false,
    starting = false,
    failing = false,
    long = false,
  }: { state: SeatState; age?: number; compact?: boolean; starting?: boolean; failing?: boolean; long?: boolean } = $props();
  const cls = $derived(starting ? 'starting' : state);
  const label = $derived(starting ? t(long ? 'state.startingFull' : 'state.starting') : t(`state.${state}`));
  const title = $derived(
    [starting ? t('state.starting.long') : t(`state.${state}.long`), failing ? t('state.failing.long') : ''].filter(Boolean).join('. '),
  );
</script>

<span class="badge st-{cls}" class:failing {title} data-state={state} data-starting={starting || undefined}>
  <span class="dot" aria-hidden="true"></span>{label}{#if failing && long}<span class="fail">· {t('state.failing')}</span>{/if}{#if age !== undefined && !compact}<span class="age">{duration(age, ui.lang)}</span>{/if}
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
  .st-starting {
    --c: var(--st-awaiting);
    --bgc: var(--st-awaiting-bg);
  }
  .failing {
    --c: var(--danger);
    --bgc: var(--danger-weak);
  }
  .fail {
    font-weight: 700;
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
