<script lang="ts">
  // Other orgs: the links of this org (active, requested, invited), what
  // each linked org opens to this org and what this org opens to it.
  import Vis from '../components/Vis.svelte';
  import { post, del, enc } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { go, href } from '../lib/router.svelte';
  import { reaches } from '../lib/openings';
  import { shortTime } from '../lib/time';
  import { visible } from '../lib/visible';
  import type { Link } from '../lib/types';

  let lookup = $state('');
  let error = $state<string | null>(null);

  $effect(() => {
    void store.links;
    void store.loadPeers();
  });

  const youOpen = (org: string) => store.grants.filter((g) => g.active && reaches(g, org)).length;

  async function act(f: () => Promise<unknown>) {
    error = null;
    try {
      await f();
      await store.openingsChanged();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function end(l: Link) {
    const key = l.state === 'active' ? 'others.confirmEnd' : l.state === 'invited' ? 'others.confirmDecline' : 'others.confirmWithdraw';
    if (confirm(t(key, { org: l.org }))) void act(() => del(`/v1/links/${enc(l.org)}`));
  }
</script>

<div class="page">
  <div class="page-head"><h1>{t('others.title')}</h1></div>
  <p class="muted intro">{t('others.intro')}</p>
  <form
    class="row"
    onsubmit={(e) => {
      e.preventDefault();
      const n = lookup.trim().toLowerCase();
      if (n) go(['orgs', n]);
    }}
  >
    <input class="mono" placeholder={t('others.lookup')} aria-label={t('others.lookup')} bind:value={lookup} />
    <button type="submit">{t('others.lookupGo')}</button>
    <span class="faint hint">{t('others.lookupHint')}</span>
  </form>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <table class="card" data-testid="links-table">
    <thead>
      <tr>
        <th>{t('admin.org')}</th><th>{t('admin.state')}</th><th>{t('others.since')}</th>
        <th title={t('others.openToYou.long')}>{t('others.openToYou')}</th><th title={t('others.youOpen.long')}>{t('others.youOpen')}</th><th>{t('others.primary')}</th><th></th>
      </tr>
    </thead>
    <tbody>
      {#if store.links.length === 0}<tr><td colspan="7" class="faint">{t('others.none')}</td></tr>{/if}
      {#each store.links as l (l.org)}
        <tr data-org={l.org}>
          <td>
            <a class="mono" href={href(['orgs', l.org])}>{l.org}</a>
            {#if l.note}<div class="note faint" title={visible(l.note)}>{l.state === 'invited' ? t('others.theirNote') : t('others.yourNote')}: <Vis text={l.note} /></div>{/if}
          </td>
          <td><span class="chip {l.state === 'active' ? 'ok' : l.state === 'invited' ? 'ack' : 'neutral'}">{t(`admin.link.${l.state}`)}</span></td>
          <td class="faint">{l.since ? shortTime(l.since, ui.now, ui.lang) : ''}</td>
          <td class="num">{#if l.state === 'active'}{store.peerCards.get(l.org)?.length ?? '…'}{:else}<span class="faint">—</span>{/if}</td>
          <td class="num">{#if l.state === 'active'}{youOpen(l.org)}{:else}<span class="faint">—</span>{/if}</td>
          <td class="mono">{#if l.primary}{l.primary}{:else}<span class="faint">—</span>{/if}</td>
          <td class="acts">
            {#if l.state === 'invited'}<button class="small primary" onclick={() => act(() => post(`/v1/links/${enc(l.org)}/accept`))}>{t('admin.accept')}</button>{/if}
            <button class="small danger" onclick={() => end(l)}>{t(l.state === 'active' ? 'others.end' : l.state === 'invited' ? 'others.decline' : 'others.withdraw')}</button>
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .intro {
    max-width: 820px;
  }
  form {
    margin: 8px 0 12px;
  }
  .hint {
    font-size: 12px;
  }
  .note {
    font-size: 12px;
    max-width: 520px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .acts {
    text-align: right;
    white-space: nowrap;
  }
</style>
