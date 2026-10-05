<script lang="ts">
  // The openings of one seat or team: to whom it is open, with remove and
  // add. Every change is previewed first (ChangePreview).
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { audiences, grantsOf, type Pending } from '../lib/openings';
  import ChangePreview from './ChangePreview.svelte';

  let {
    target,
    team,
    inherited = [],
  }: {
    target: string;
    /** For a contact seat: its team (whose openings also open the seat). */
    team?: string;
    /** Audiences the seat is open to through its team. */
    inherited?: string[];
  } = $props();

  const own = $derived(grantsOf(store.grants, target));
  const choices = $derived(audiences(store.links).filter((a) => !own.some((g) => g.audience === a)));
  let pick = $state('linked');
  let pending = $state.raw<Pending | null>(null);
  const aud = (a: string) => (a === 'linked' ? t('open.linked') : a);

  $effect(() => {
    if (!choices.includes(pick) && choices.length) pick = choices[0];
  });
</script>

<div class="oe" data-testid="openings-editor" data-target={target}>
  <div class="row wrap chips">
    {#if own.length === 0 && inherited.length === 0}<span class="faint">{t('open.closed')}</span>{/if}
    {#each own as g (g.id)}
      <span class="chip {g.active ? 'ok' : 'neutral'}" data-audience={g.audience} title={g.active ? t('open.active.long') : t('open.waiting.long')}>
        {aud(g.audience)}{#if !g.active} · {t('open.waiting')}{/if}{#if g.managed} · {t('open.managed')}{/if}
        <button class="x" aria-label={t('open.close', { audience: aud(g.audience) })} onclick={() => (pending = { remove: g })} disabled={!!pending}>✕</button>
      </span>
    {/each}
    {#each inherited as a}
      <span class="chip neutral" title={t('open.viaTeam.long', { team: team ?? '' })}>{aud(a)} · <a href={href(['teams', team ?? ''])}>{t('open.viaTeam', { team: team ?? '' })}</a></span>
    {/each}
  </div>
  {#if !pending && choices.length}
    <div class="row add">
      <select bind:value={pick} aria-label={t('open.audience')}>
        {#each choices as a}<option value={a}>{aud(a)}</option>{/each}
      </select>
      <button class="small" onclick={() => (pending = { add: { target, audience: pick } })} data-testid="open-add">{t('open.open')}</button>
    </div>
  {/if}
  {#if pending}
    <ChangePreview change={pending} onclose={() => (pending = null)} />
  {/if}
</div>

<style>
  .oe {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }
  .chips {
    gap: 4px;
  }
  .chip .x {
    border: none;
    background: transparent;
    padding: 0 0 0 4px;
    color: inherit;
    font-size: 11px;
    line-height: 1;
  }
  .add select {
    max-width: 220px;
  }
</style>
