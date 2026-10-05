<script lang="ts">
  import Vis from '../components/Vis.svelte';
  import OpeningsEditor from '../components/OpeningsEditor.svelte';
  import { put, patch, enc } from '../lib/api';
  import { activeOrgs, contactsFor } from '../lib/openings';
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { go, href } from '../lib/router.svelte';
  import { teamRows } from '../lib/teams';
  import { visible } from '../lib/visible';

  let { selected }: { selected?: string } = $props();

  const rows = $derived(teamRows(store.teams, store.seats));
  const team = $derived(selected ? store.teams.find((x) => x.name === selected) : undefined);
  const aud = (a: string) => (a === 'linked' ? t('open.linked') : a);

  let desc = $state('');
  let contacts = $state.raw<Set<string>>(new Set());
  let msg = $state<string | null>(null);
  let error = $state<string | null>(null);

  // Reset the form when another team is selected or the team changed.
  $effect(() => {
    desc = team?.description ?? '';
    contacts = new Set(team?.contacts ?? []);
    msg = null;
    error = null;
  });

  function toggleContact(m: string) {
    const next = new Set(contacts);
    if (next.has(m)) next.delete(m);
    else next.add(m);
    contacts = next;
  }

  // The team's own primary contacts per linked org.
  let pOrg = $state('');
  let pSeat = $state('');
  const pChoices = $derived(team && pOrg ? contactsFor(store.seats.filter((s) => team.members.includes(s.name)), pOrg) : []);
  async function setPrimary(org: string, seat: string | null) {
    if (!team) return;
    msg = null;
    error = null;
    try {
      await patch(`/v1/links/${enc(org)}`, { primary: seat, team: team.name });
      await store.openingsChanged();
      pSeat = '';
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (!team) return;
    msg = null;
    error = null;
    try {
      await put(`/v1/teams/${enc(team.name)}`, { description: desc.trim() || null, contacts: team.members.filter((m) => contacts.has(m)) });
      await store.openingsChanged();
      msg = t('admin.saved');
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }
</script>

<div class="teams" class:split={!!team}>
  <section class="list" aria-label={t('team.title')}>
    <div class="page-head"><h1>{t('team.title')}</h1><span class="faint">{t('team.count', { n: store.teams.length })}</span></div>
    <p class="faint intro">{t('team.intro')}</p>
    <div class="forest card" role="tree" aria-label={t('team.title')}>
      {#if rows.length === 0}<div class="empty">{t('org.empty')}</div>{/if}
      {#each rows as r, i (r.type === 'loose' ? '~loose' : `${r.type}:${r.name}:${i}`)}
        {#if r.type === 'team'}
          <a
            class="r team"
            class:sel={r.name === selected}
            role="treeitem"
            aria-selected={r.name === selected}
            data-team={r.name}
            href={href(['teams', r.name])}
            style="padding-left: {r.depth * 18 + 8}px"
          >
            <span class="chip neutral">{t('team.team')}</span>
            <span class="mono nm">{r.name}</span>
            {#if r.team.open_to?.length}<span class="chip open" title={t('open.to', { list: r.team.open_to.map(aud).join(', ') })}>{t('open.chip')} · {r.team.open_to.map(aud).join(', ')}</span>{/if}
            {#if r.team.managed}<span class="faint small">{t('open.managed')}</span>{/if}
            {#if r.team.description}<span class="desc faint">{visible(r.team.description)}</span>{/if}
          </a>
        {:else if r.type === 'loose'}
          <div class="r loose" role="treeitem" aria-selected="false">{t('team.loose')}</div>
        {:else}
          {@const s = store.seatMap.get(r.name)}
          <a class="r seat" role="treeitem" aria-selected="false" data-seat={r.name} href={href(['org', r.name])} style="padding-left: {r.depth * 18 + 8}px">
            <span class="mono nm" class:user={s?.kind === 'user'}>{r.name}</span>
            {#if r.root}<span class="chip neutral" title={t('team.root.long')}>{t('team.root')}</span>{/if}
            {#if r.contact}<span class="chip neutral" title={t('team.contact.long')}>{t('team.contact')}</span>{/if}
            {#if s?.open_to?.length}<span class="chip open" title={t('open.to', { list: s.open_to.map(aud).join(', ') })}>{t('open.chip')}</span>{/if}
            {#if s?.description}<span class="desc faint">{visible(s.description)}</span>{/if}
          </a>
        {/if}
      {/each}
    </div>
  </section>
  {#if selected}
    <section class="panel" aria-label={selected}>
      {#if !team}
        <div class="empty">{t('team.notFound', { name: selected })}</div>
      {:else}
        <div class="head">
          <h2 class="mono">{team.name}</h2>
          {#if team.managed}<span class="chip neutral" title={t('admin.managed')}>{t('open.managed')}</span>{/if}
          <span class="spacer"></span>
          <button class="ghost small" aria-label="close" onclick={() => go(['teams'])}>✕</button>
        </div>
        <dl class="facts">
          <dt>{t('team.rootSeat')}</dt>
          <dd>{#if team.root}<a class="mono" href={href(['org', team.root])}>{team.root}</a>{:else}<span class="faint">{t('seat.none')}</span>{/if}</dd>
          <dt>{t('team.parent')}</dt>
          <dd>{#if team.parent}<a class="mono" href={href(['teams', team.parent])}>{team.parent}</a>{:else}<span class="faint">{t('seat.none')}</span>{/if}</dd>
          <dt>{t('open.title')}</dt>
          <dd>
            {#if team.contacts.length}
              <OpeningsEditor target={team.name} />
            {:else}
              <span class="faint">{t('team.needsContact')}</span>
            {/if}
          </dd>
          <dt title={t('team.primaries.long')}>{t('team.primaries')}</dt>
          <dd data-testid="team-primaries">
            {#each team.primaries ?? [] as p (p.org)}
              <div class="row">
                <a class="mono" href={href(['orgs', p.org])}>{p.org}</a>: <a class="mono" href={href(['org', p.seat])}>{p.seat}</a>
                <button class="small ghost" onclick={() => setPrimary(p.org, null)}>{t('team.primaryClear')}</button>
              </div>
            {/each}
            {#if activeOrgs(store.links).length}
              <div class="row">
                <select bind:value={pOrg} aria-label={t('team.primaryOrg')}>
                  <option value="">{t('team.primaryOrg')}…</option>
                  {#each activeOrgs(store.links) as o}<option value={o}>{o}</option>{/each}
                </select>
                <select bind:value={pSeat} aria-label={t('seat.team')} disabled={!pChoices.length}>
                  <option value="">—</option>
                  {#each pChoices as c}<option value={c}>{c}</option>{/each}
                </select>
                <button class="small" disabled={!pSeat} onclick={() => setPrimary(pOrg, pSeat)}>{t('peer.primarySet')}</button>
              </div>
            {:else if !(team.primaries ?? []).length}<span class="faint">{t('admin.none')}</span>{/if}
          </dd>
        </dl>
        <form class="card form" onsubmit={save} data-testid="team-form">
          <label class="col"><span>{t('seat.description')}</span>
            <textarea rows="2" maxlength="500" bind:value={desc}></textarea></label>
          <fieldset>
            <legend>{t('team.members')} · {t('team.contactsHint')}</legend>
            {#each team.members as m}
              <label class="check">
                <input type="checkbox" checked={contacts.has(m)} onchange={() => toggleContact(m)} />
                <a class="mono" href={href(['org', m])}>{m}</a>
                {#if store.seatMap.get(m)?.description}<span class="faint desc"><Vis text={store.seatMap.get(m)?.description ?? ''} /></span>{/if}
              </label>
            {/each}
            {#if team.members.length === 0}<span class="faint">{t('admin.none')}</span>{/if}
          </fieldset>
          <div class="row"><button class="primary" type="submit">{t('admin.save')}</button>{#if msg}<span class="ok">{msg}</span>{/if}</div>
          {#if error}<p class="error" role="alert">{error}</p>{/if}
        </form>
      {/if}
    </section>
  {/if}
</div>

<style>
  .teams {
    display: grid;
    grid-template-columns: 1fr;
    height: 100%;
    min-height: 0;
  }
  .teams.split {
    grid-template-columns: minmax(380px, 52%) 1fr;
  }
  .list {
    padding: 12px 16px;
    overflow: auto;
    min-width: 0;
  }
  .intro {
    margin: 0 0 8px;
    font-size: 12px;
  }
  .forest {
    display: flex;
    flex-direction: column;
  }
  .r {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 28px;
    padding-right: 10px;
    border-bottom: 1px solid var(--border);
    color: var(--text);
    min-width: 0;
  }
  .r:hover {
    background: var(--surface-2);
    text-decoration: none;
  }
  .r.team {
    background: var(--surface-2);
    font-weight: 600;
  }
  .r.team.sel {
    background: var(--accent-weak);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .r.loose {
    padding-left: 8px;
    color: var(--muted);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    background: var(--surface-2);
  }
  .nm {
    font-size: 12.5px;
    white-space: nowrap;
  }
  .nm.user {
    color: var(--accent);
  }
  .desc {
    font-weight: 400;
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .small {
    font-size: 11px;
    font-weight: 400;
  }
  .chip {
    flex: none;
  }
  .chip.open {
    color: var(--ok);
    background: var(--ok-weak);
  }
  .panel {
    border-left: 1px solid var(--border);
    padding: 12px 16px;
    overflow: auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  h2 {
    font-size: 17px;
    margin: 0;
  }
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 5px 14px;
    margin: 0;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    min-width: 0;
  }
  .form {
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  label.col {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  label.col span {
    color: var(--muted);
  }
  textarea {
    width: 100%;
    font-family: var(--font);
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 6px 10px 8px;
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  legend {
    color: var(--muted);
    padding: 0 4px;
  }
  label.check {
    display: flex;
    gap: 6px;
    align-items: center;
    min-width: 0;
  }
  @media (max-width: 1000px) {
    .teams.split {
      grid-template-columns: 1fr;
    }
    .teams.split .list {
      display: none;
    }
  }
</style>
