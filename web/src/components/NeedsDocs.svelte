<script lang="ts">
  // Documents in "needs you": stale documents the user owns (verify) and
  // drafts that wait for the user to activate them: `decision` drafts, and
  // the drafts of teams whose documents the user activates.
  import Vis from './Vis.svelte';
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { docHref, docId } from '../lib/docs';
  import { docAction, docError, listDocs, patchDoc } from '../lib/docops';
  import type { Doc } from '../lib/types';

  let stale = $state.raw<Doc[]>([]);
  let drafts = $state.raw<Doc[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let gen = 0;

  async function load() {
    const me = store.me?.seat;
    if (!me) return;
    const g = ++gen;
    try {
      const humanTeams = store.teams.filter((x) => x.doc_activate_by === 'human').map((x) => x.name);
      const [s, d, ...teams] = await Promise.all([
        listDocs({ owner: me, stale: '1', limit: 100 }),
        listDocs({ kind: 'decision', status: 'draft', limit: 100 }),
        ...humanTeams.map((team) => listDocs({ team, status: 'draft', limit: 100 })),
      ]);
      if (g !== gen) return;
      stale = s.docs;
      const seen = new Map<number, Doc>();
      for (const x of [d, ...teams]) for (const doc of x.docs) seen.set(doc.num, doc);
      drafts = [...seen.values()].sort((a, b) => b.num - a.num);
    } catch {
      // The mail queue still works without the document lists.
    }
  }

  $effect(() => {
    void store.docSeq;
    void store.teams;
    void load();
  });

  async function run(f: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await f();
      await load();
    } catch (e) {
      error = docError(e);
    } finally {
      busy = false;
    }
  }
</script>

{#if stale.length || drafts.length}
  <div class="ndocs" data-testid="needs-docs">
    {#if error}<p class="error">{error}</p>{/if}
    {#if drafts.length}
      <h3>{t('needs.docsDraft')} <span class="count alert">{drafts.length}</span></h3>
      <ul>
        {#each drafts as d (d.num)}
          <li data-doc={d.num}>
            <a class="mono" href={docHref(d.num)}>{docId(d.num)}</a>
            <a class="title" href={docHref(d.num)}><Vis text={d.title} /></a>
            <span class="chip neutral">{d.kind}</span>
            <span class="faint">{d.owner}</span>
            <button class="small" disabled={busy} onclick={() => run(() => patchDoc(d.num, { status: 'active' }))}>{t('doc.activate')}</button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if stale.length}
      <h3>{t('needs.docsStale')} <span class="count alert">{stale.length}</span></h3>
      <ul>
        {#each stale as d (d.num)}
          <li data-doc={d.num}>
            <a class="mono" href={docHref(d.num)}>{docId(d.num)}</a>
            <a class="title" href={docHref(d.num)}><Vis text={d.title} /></a>
            <span class="chip neutral">{d.kind}</span>
            <button class="small" disabled={busy} onclick={() => run(() => docAction(d.num, 'verify'))}>{t('doc.verify')}</button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .ndocs {
    padding: 8px 14px;
    border-bottom: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  h3 {
    font-size: 12px;
    color: var(--muted);
  }
  ul {
    list-style: none;
    margin: 0 0 4px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  li {
    display: flex;
    gap: 6px;
    align-items: baseline;
    flex-wrap: wrap;
    min-width: 0;
  }
  .title {
    overflow-wrap: anywhere;
  }
</style>
