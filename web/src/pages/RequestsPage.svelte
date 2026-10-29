<script lang="ts">
  // The org's requests for the person with filters (`#/requests?state=all
  // &kind=&seat=&class=`): the history of what was asked and answered.
  import Vis from '../components/Vis.svelte';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { go, href } from '../lib/router.svelte';
  import { fullTime, shortTime } from '../lib/time';
  import { CLASSES, KINDS, STATES, answerText, parseFilters, requestId, type RequestFilters } from '../lib/requests';
  import { listRequests, requestError } from '../lib/requestops';
  import { taskId } from '../lib/tasks';
  import type { Request } from '../lib/types';

  let { query }: { query: URLSearchParams } = $props();

  const f = $derived(parseFilters(query));
  const agents = $derived(store.seats.filter((s) => s.kind === 'agent').map((s) => s.name));

  let rows = $state.raw<Request[]>([]);
  let truncated = $state(false);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let gen = 0;

  async function load(filters: RequestFilters) {
    const g = ++gen;
    loading = true;
    try {
      const r = await listRequests({ state: filters.state, kind: filters.kind || undefined, seat: filters.seat || undefined, class: filters.cls || undefined, limit: 500 });
      if (g !== gen) return;
      rows = r.requests;
      truncated = !!r.truncated;
      error = null;
    } catch (e) {
      if (g === gen) error = requestError(e);
    } finally {
      if (g === gen) loading = false;
    }
  }

  // Load when the filters change and on every round of request events.
  $effect(() => {
    void store.requestSeq;
    void load(f);
  });

  function set(key: 'state' | 'kind' | 'seat' | 'class', value: string) {
    const next = { state: f.state, kind: f.kind, seat: f.seat, class: f.cls, [key]: value };
    go(['requests'], next, true);
  }
</script>

<div class="page reqs">
  <div class="page-head">
    <h1>{t('req.list.title')}</h1>
    <span class="spacer"></span>
    <a href={href(['needs'])}>{t('nav.needs')} →</a>
  </div>
  <p class="muted intro">{t('req.list.intro')}</p>
  <div class="filters row wrap" data-testid="req-filters">
    <label>{t('req.state')}
      <select value={f.state} onchange={(e) => set('state', e.currentTarget.value)} aria-label={t('req.state')}>
        <option value="all">{t('req.any')}</option>
        {#each STATES as s}<option value={s}>{t(`req.state.${s}`)}</option>{/each}
      </select>
    </label>
    <label>{t('req.kind')}
      <select value={f.kind} onchange={(e) => set('kind', e.currentTarget.value)} aria-label={t('req.kind')}>
        <option value="">{t('req.any')}</option>
        {#each KINDS as k}<option value={k}>{t(`req.kind.${k}`)}</option>{/each}
      </select>
    </label>
    <label>{t('req.class')}
      <select value={f.cls} onchange={(e) => set('class', e.currentTarget.value)} aria-label={t('req.class')}>
        <option value="">{t('req.any')}</option>
        {#each CLASSES as c}<option value={c}>{t(`req.class.${c}`)}</option>{/each}
      </select>
    </label>
    <label>{t('req.seat')}
      <select value={f.seat} onchange={(e) => set('seat', e.currentTarget.value)} aria-label={t('req.seat')}>
        <option value="">{t('req.any')}</option>
        {#each agents as a}<option value={a}>{a}</option>{/each}
        {#if f.seat && !agents.includes(f.seat)}<option value={f.seat}>{f.seat}</option>{/if}
      </select>
    </label>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <div class="tablewrap card">
    <table data-testid="req-table">
      <thead>
        <tr><th>#</th><th>{t('req.title')}</th><th>{t('req.state')}</th><th>{t('req.class')}</th><th>{t('req.seat')}</th><th>{t('req.answer')}</th><th>{t('req.created')}</th></tr>
      </thead>
      <tbody>
        {#if !loading && rows.length === 0}<tr><td colspan="7" class="faint">{t('req.none')}</td></tr>{/if}
        {#each rows as r (r.num)}
          <tr data-request={r.num}>
            <td class="mono"><a href={href(['requests', requestId(r.num)])}>{requestId(r.num)}</a></td>
            <td class="title">
              <a href={href(['requests', requestId(r.num)])}><Vis text={r.title} /></a>
              {#if r.kind !== 'question'}<span class="chip neutral">{t(`req.kind.${r.kind}`)}</span>{/if}
              {#if r.task}<a class="mono" href={href(['tasks', taskId(r.task)])}>{taskId(r.task)}</a>{/if}
            </td>
            <td><span class="chip st-{r.state}">{t(`req.state.${r.state}`)}</span></td>
            <td>{t(`req.class.${r.class}`)}</td>
            <td class="mono"><a href={href(['org', r.seat])}>{r.seat}</a></td>
            <td class="ans">{#if r.answer}<Vis text={answerText(r)} /> <span class="faint">({r.answer.by})</span>{/if}</td>
            <td class="faint" title={fullTime(r.created_at, ui.lang)}>{shortTime(r.created_at, ui.now, ui.lang)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  {#if truncated}<p class="faint">{t('req.truncated', { n: rows.length })}</p>{/if}
</div>

<style>
  .intro {
    margin: 0 0 8px;
  }
  .filters {
    gap: 8px 14px;
    margin-bottom: 10px;
  }
  .filters label {
    display: flex;
    gap: 6px;
    align-items: center;
    color: var(--muted);
  }
  .tablewrap {
    overflow-x: auto;
  }
  table {
    width: 100%;
  }
  .title {
    overflow-wrap: anywhere;
    min-width: 180px;
  }
  .title .chip,
  .title .mono {
    margin-left: 6px;
  }
  .ans {
    overflow-wrap: anywhere;
    max-width: 260px;
  }
  .st-open {
    color: var(--warn);
    background: var(--warn-weak);
  }
  .st-answered,
  .st-expired {
    color: var(--ok);
  }
</style>
