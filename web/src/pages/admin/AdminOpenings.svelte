<script lang="ts">
  // Every grant of the org in one table, with remove and add; each change
  // is previewed before it is applied.
  import ChangePreview from '../../components/ChangePreview.svelte';
  import { store } from '../../lib/store.svelte';
  import { t, ui } from '../../lib/ui.svelte';
  import { href } from '../../lib/router.svelte';
  import { audiences, type Pending } from '../../lib/openings';
  import { shortTime } from '../../lib/time';

  let pending = $state.raw<Pending | null>(null);
  let target = $state('');
  let audience = $state('linked');

  const targets = $derived([
    ...store.seats.map((s) => ({ name: s.name, team: false, disabled: false })),
    ...store.teams.map((x) => ({ name: x.name, team: true, disabled: x.contacts.length === 0 })),
  ]);
  const choices = $derived(audiences(store.links));
  const aud = (a: string) => (a === 'linked' ? t('open.linked') : a);
  const sorted = $derived([...store.grants].sort((a, b) => a.target.localeCompare(b.target) || a.audience.localeCompare(b.audience)));
</script>

<div class="page">
  <div class="page-head"><h1>{t('nav.admin')} · {t('nav.openings')}</h1><span class="faint">{t('open.count', { n: store.grants.length })}</span></div>
  <p class="muted intro">{t('open.intro')}</p>
  <div class="card addbox">
    {#if !pending}
      <div class="row wrap">
        <select bind:value={target} aria-label={t('open.target')} data-testid="grant-target">
          <option value="">{t('peer.pickTarget')}</option>
          {#each targets as x}<option value={x.name} disabled={x.disabled}>{x.name}{x.team ? ` (${t('card.team')})` : ''}</option>{/each}
        </select>
        <span class="faint">→</span>
        <select bind:value={audience} aria-label={t('open.audience')} data-testid="grant-audience">
          {#each choices as a}<option value={a}>{aud(a)}</option>{/each}
        </select>
        <button class="primary small" disabled={!target} onclick={() => (pending = { add: { target, audience } })} data-testid="open-add">{t('open.preview')}</button>
      </div>
    {:else}
      <ChangePreview
        change={pending}
        onclose={(applied) => {
          pending = null;
          if (applied) target = '';
        }}
      />
    {/if}
  </div>
  <table class="card" data-testid="grants-table">
    <thead>
      <tr><th>{t('open.target')}</th><th>{t('admin.kind')}</th><th>{t('open.audience')}</th><th>{t('admin.state')}</th><th>{t('admin.created')}</th><th></th></tr>
    </thead>
    <tbody>
      {#if sorted.length === 0}<tr><td colspan="6" class="faint">{t('open.none')}</td></tr>{/if}
      {#each sorted as g (g.id)}
        <tr data-grant={g.id} data-target={g.target}>
          <td><a class="mono" href={href(g.kind === 'team' ? ['teams', g.target] : ['org', g.target])}>{g.target}</a></td>
          <td><span class="chip neutral">{t(`card.${g.kind}`)}</span></td>
          <td>{#if g.audience === 'linked'}{aud(g.audience)}{:else}<a class="mono" href={href(['orgs', g.audience])}>{g.audience}</a>{/if}</td>
          <td>
            <span class="chip {g.active ? 'ok' : 'neutral'}" title={g.active ? t('open.active.long') : t('open.waiting.long')}>{g.active ? t('open.active') : t('open.waiting')}</span>
            {#if g.managed}<span class="faint" title={t('admin.managed')}>· {t('open.managed')}</span>{/if}
          </td>
          <td class="faint">{shortTime(g.created_at, ui.now, ui.lang)}</td>
          <td class="acts"><button class="small ghost danger" disabled={!!pending} onclick={() => (pending = { remove: g })}>{t('open.remove')}</button></td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .intro {
    max-width: 820px;
  }
  .addbox {
    padding: 8px 12px;
    margin-bottom: 12px;
    max-width: 820px;
  }
  .addbox select {
    max-width: 260px;
  }
  .acts {
    text-align: right;
    white-space: nowrap;
  }
</style>
