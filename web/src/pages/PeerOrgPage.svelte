<script lang="ts">
  // One other org as this org sees it: its card, the link, what it opens
  // to this org and what this org opens to it.
  import Vis from '../components/Vis.svelte';
  import ChangePreview from '../components/ChangePreview.svelte';
  import { get, post, del, enc, ApiError } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { fullTime } from '../lib/time';
  import type { Pending } from '../lib/openings';
  import type { PeerOrg } from '../lib/types';

  let { name }: { name: string } = $props();

  let peer = $state.raw<PeerOrg | null>(null);
  let missing = $state(false);
  let error = $state<string | null>(null);
  let actError = $state<string | null>(null);
  let note = $state('');
  let reload = $state(0);
  let pending = $state.raw<Pending | null>(null);
  let pick = $state('');

  $effect(() => {
    const n = name;
    void reload;
    void store.grants;
    void store.links;
    error = null;
    get<PeerOrg>(`/v1/orgs/${enc(n)}`)
      .then((p) => {
        if (n !== name) return;
        peer = p;
        missing = false;
      })
      .catch((e) => {
        if (n !== name) return;
        peer = null;
        if (e instanceof ApiError && e.status === 404) missing = true;
        else error = e instanceof Error ? e.message : String(e);
      });
  });

  const link = $derived(peer?.link ?? store.links.find((l) => l.org === name));
  const active = $derived(link?.state === 'active');
  const aud = (a: string) => (a === 'linked' ? t('open.linked') : a);
  // Seats and teams that could still be opened to this org by name.
  const openable = $derived.by(() => {
    const has = new Set(store.grants.filter((g) => g.audience === name).map((g) => g.target));
    const teams = store.teams.filter((x) => x.contacts.length > 0).map((x) => x.name);
    return [...store.seats.map((s) => s.name), ...teams].filter((x) => !has.has(x));
  });

  async function act(f: () => Promise<unknown>) {
    actError = null;
    try {
      await f();
      await store.openingsChanged();
      reload++;
    } catch (e) {
      actError = e instanceof Error ? e.message : String(e);
    }
  }

  function request(e: SubmitEvent) {
    e.preventDefault();
    const body: { org: string; note?: string } = { org: name };
    if (note.trim()) body.note = note.trim();
    void act(async () => {
      await post('/v1/links', body);
      note = '';
    });
  }

  function end() {
    if (!link) return;
    const key = link.state === 'active' ? 'others.confirmEnd' : link.state === 'invited' ? 'others.confirmDecline' : 'others.confirmWithdraw';
    if (confirm(t(key, { org: name }))) void act(() => del(`/v1/links/${enc(name)}`));
  }
</script>

<div class="page peer" data-testid="peer-org">
  <div class="page-head">
    <a href={href(['orgs'])} class="faint">{t('others.title')}</a><span class="faint">/</span>
    <h1 class="mono">{name}</h1>
    {#if peer?.card.display_name}<span class="muted"><Vis text={peer.card.display_name} /></span>{/if}
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}

  {#if peer?.card.description}
    <p class="card-desc" data-testid="org-description"><Vis text={peer.card.description} /></p>
  {:else if missing}
    <p class="faint" data-testid="org-missing">{t('peer.missing')}</p>
  {:else if peer}
    <p class="faint">{t('peer.noDescription')}</p>
  {/if}

  <section class="card box" aria-labelledby="h-link">
    <h2 id="h-link">{t('peer.link')}</h2>
    {#if link}
      <div class="row wrap">
        <span class="chip {link.state === 'active' ? 'ok' : link.state === 'invited' ? 'ack' : 'neutral'}" data-testid="link-state">{t(`admin.link.${link.state}`)}</span>
        {#if link.since}<span class="faint">{t(link.state === 'active' ? 'peer.activeSince' : 'peer.requestedAt', { time: fullTime(link.since, ui.lang) })}</span>{/if}
        <span class="spacer"></span>
        {#if link.state === 'invited'}<button class="small primary" onclick={() => act(() => post(`/v1/links/${enc(name)}/accept`))}>{t('admin.accept')}</button>{/if}
        <button class="small danger" onclick={end}>{t(link.state === 'active' ? 'others.end' : link.state === 'invited' ? 'others.decline' : 'others.withdraw')}</button>
      </div>
      {#if link.note}
        <p class="note"><span class="faint">{link.state === 'invited' ? t('others.theirNote') : t('others.yourNote')}:</span> <span class="untrusted"><Vis text={link.note} /></span></p>
      {/if}
      {#if !active}<p class="faint">{t('peer.notActive')}</p>{/if}
    {:else}
      <p class="faint">{t('peer.noLink')}</p>
      <form class="request" onsubmit={request} data-testid="request-link">
        <textarea rows="2" maxlength="500" placeholder={t('peer.notePlaceholder')} aria-label={t('peer.note')} bind:value={note}></textarea>
        <div class="row"><button class="primary" type="submit">{t('admin.requestLink')}</button><span class="faint">{t('peer.noteHint', { n: 500 - note.length })}</span></div>
      </form>
    {/if}
    {#if actError}<p class="error" role="alert">{actError}</p>{/if}
  </section>

  {#if active && peer}
    <div class="cols">
      <section class="card box" aria-labelledby="h-in" data-testid="open-to-you">
        <h2 id="h-in">{t('peer.openToYou', { org: name })}</h2>
        <p class="faint sub">{t('peer.openToYou.long')}</p>
        <table>
          <thead><tr><th>{t('peer.address')}</th><th>{t('admin.kind')}</th><th>{t('seat.description')}</th><th></th></tr></thead>
          <tbody>
            {#if peer.open_to_you.length === 0}<tr><td colspan="4" class="faint">{t('peer.nothingOpen')}</td></tr>{/if}
            {#each peer.open_to_you as c (c.address)}
              <tr data-card={c.address}>
                <td class="mono">{c.address}</td>
                <td><span class="chip neutral">{t(`card.${c.kind}`)}</span></td>
                <td class="desc">{#if c.description}<Vis text={c.description} />{/if}</td>
                <td class="acts">{#if c.kind !== 'team'}<a href={href(['compose'], { to: c.address })}>{t('seat.write')}</a>{/if}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>
      <section class="card box" aria-labelledby="h-out" data-testid="you-open">
        <h2 id="h-out">{t('peer.youOpen', { org: name })}</h2>
        <p class="faint sub">{t('peer.youOpen.long')}</p>
        <table>
          <thead><tr><th>{t('open.target')}</th><th>{t('admin.kind')}</th><th>{t('open.audience')}</th><th></th></tr></thead>
          <tbody>
            {#if peer.you_open.length === 0}<tr><td colspan="4" class="faint">{t('peer.nothingOpen')}</td></tr>{/if}
            {#each peer.you_open as g (g.id)}
              <tr data-grant={g.id}>
                <td><a class="mono" href={href(g.kind === 'team' ? ['teams', g.target] : ['org', g.target])}>{g.target}</a></td>
                <td><span class="chip neutral">{t(`card.${g.kind}`)}</span></td>
                <td>{aud(g.audience)}{#if g.managed} <span class="faint">· {t('open.managed')}</span>{/if}</td>
                <td class="acts"><button class="small ghost danger" disabled={!!pending} onclick={() => (pending = { remove: g })}>{t('open.remove')}</button></td>
              </tr>
            {/each}
          </tbody>
        </table>
        {#if !pending}
          <div class="row add">
            <select bind:value={pick} aria-label={t('open.target')}>
              <option value="">{t('peer.pickTarget')}</option>
              {#each openable as x}<option value={x}>{x}</option>{/each}
            </select>
            <button class="small" disabled={!pick} onclick={() => (pending = { add: { target: pick, audience: name } })} data-testid="open-add">{t('peer.openTo', { org: name })}</button>
          </div>
        {:else}
          <ChangePreview
            change={pending}
            onclose={(applied) => {
              pending = null;
              if (applied) {
                pick = '';
                reload++;
              }
            }}
          />
        {/if}
      </section>
    </div>
  {/if}
</div>

<style>
  .peer {
    max-width: 1200px;
  }
  .page-head a {
    font-size: 13px;
  }
  .card-desc {
    margin: 0 0 10px;
    max-width: 820px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .box {
    padding: 10px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 12px;
    min-width: 0;
  }
  h2 {
    font-size: 14px;
    margin: 0;
  }
  .sub {
    margin: -2px 0 2px;
    font-size: 12px;
  }
  .note {
    margin: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .untrusted {
    font-style: italic;
  }
  .request {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 620px;
  }
  .request textarea {
    width: 100%;
    font-family: var(--font);
  }
  .cols {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
    align-items: start;
  }
  .cols .box {
    margin-bottom: 0;
  }
  table {
    border: none;
  }
  .desc {
    word-break: break-word;
  }
  .acts {
    text-align: right;
    white-space: nowrap;
  }
  .add select {
    max-width: 240px;
  }
  @media (max-width: 1100px) {
    .cols {
      grid-template-columns: 1fr;
    }
  }
</style>
