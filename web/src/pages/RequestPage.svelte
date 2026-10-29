<script lang="ts">
  // One request for the person (`#/requests/r12`): its fields, the answer
  // form while it is open, its answer, its history and its threads.
  import Vis from '../components/Vis.svelte';
  import RequestCard from '../components/RequestCard.svelte';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { fullTime, shortTime } from '../lib/time';
  import { optionOf, requestId } from '../lib/requests';
  import { getRequest, requestError, withdrawRequest } from '../lib/requestops';
  import { taskId } from '../lib/tasks';
  import type { Request, RequestDetail } from '../lib/types';

  let { num }: { num: number } = $props();

  let detail = $state.raw<RequestDetail | null>(null);
  let loadError = $state<string | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let gen = 0;

  async function load() {
    const g = ++gen;
    try {
      const d = await getRequest(num);
      if (g !== gen) return;
      detail = d;
      loadError = null;
    } catch (e) {
      if (g === gen) loadError = requestError(e);
    }
  }

  // Load on open and on every round of request events that may concern it.
  let first = true;
  $effect(() => {
    void store.requestSeq;
    const changed = store.requestChanged;
    if (first || !changed || changed.has(num)) void load();
    first = false;
  });

  const r = $derived(detail?.request);

  function resolved(next: Request) {
    if (detail) detail = { ...detail, request: next };
    void load();
  }

  async function withdraw() {
    if (!r || busy || !confirm(t('req.withdraw.confirm', { id: requestId(r.num) }))) return;
    busy = true;
    error = null;
    try {
      detail = await withdrawRequest(r.num);
    } catch (e) {
      error = requestError(e);
      void load();
    } finally {
      busy = false;
    }
  }
</script>

<div class="page reqpage">
  <p class="back"><a href={href(['requests'], { state: 'all' })}>← {t('req.list.title')}</a> · <a href={href(['needs'])}>{t('nav.needs')}</a></p>
  {#if loadError && !detail}
    <p class="error" role="alert">{t('req.notFound', { id: requestId(num) })} ({loadError})</p>
  {:else if !r || !detail}
    <p class="faint">{t('app.loading')}</p>
  {:else}
    <div class="page-head">
      <h1><span class="mono faint">{requestId(r.num)}</span> <Vis text={r.title} /></h1>
      <span class="chip st-{r.state}" data-testid="req-state">{t(`req.state.${r.state}`)}</span>
      <span class="spacer"></span>
      {#if r.state === 'open'}<button class="small danger" disabled={busy} onclick={withdraw}>{t('req.withdraw')}</button>{/if}
    </div>
    {#if error}<p class="error" role="alert">{error}</p>{/if}

    {#key r.num}
      <RequestCard request={r} mode="page" onresolved={resolved} />
    {/key}

    <dl class="facts card" data-testid="req-facts">
      <dt>{t('req.kind')}</dt>
      <dd>{t(`req.kind.${r.kind}`)}{#if r.attention} · <span class="mono">{r.attention}</span>{/if}</dd>
      <dt>{t('req.class')}</dt>
      <dd>{t(`req.class.${r.class}`)} <span class="faint">({t(`req.class.${r.class}.long`)})</span></dd>
      <dt>{t('req.seat')}</dt>
      <dd><a class="mono" href={href(['org', r.seat])}>{r.seat}</a>{#if r.asker && r.asker !== r.seat} · {t('req.askedBy', { seat: r.asker })}{/if}</dd>
      {#if r.task}
        <dt>{t('req.task')}</dt>
        <dd><a class="mono" href={href(['tasks', taskId(r.task)])}>{taskId(r.task)}</a></dd>
      {/if}
      <dt>{t('req.created')}</dt>
      <dd title={fullTime(r.created_at, ui.lang)}>{shortTime(r.created_at, ui.now, ui.lang)} · {t('req.source')} <span class="mono">{r.source}</span></dd>
      {#if r.deadline}
        <dt>{t('req.deadline')}</dt>
        <dd title={fullTime(r.deadline, ui.lang)}>{shortTime(r.deadline, ui.now, ui.lang)}</dd>
      {/if}
      <dt>{t('req.default')}</dt>
      <dd>{#if optionOf(r, r.default)}<Vis text={optionOf(r, r.default)?.label ?? ''} /> <span class="mono faint">{r.default}</span>{:else}<span class="faint">{t('req.noDefault')}</span>{/if}</dd>
      {#if r.thread}
        <dt>{t('req.thread')}</dt>
        <dd><a href={href(['thread', r.thread])} data-testid="req-thread">{t('thread.title', { id: r.thread })}</a></dd>
      {/if}
      {#if detail.threads?.length}
        <dt>{t('req.threads')}</dt>
        <dd>{#each detail.threads as th}<a href={href(['thread', th])}>#{th}</a>{/each}</dd>
      {/if}
    </dl>

    <section class="events">
      <h2>{t('req.events')}</h2>
      <ol data-testid="req-events">
        {#each detail.events ?? [] as ev (ev.seq)}
          <li>
            <span class="faint when" title={fullTime(ev.at, ui.lang)}>{shortTime(ev.at, ui.now, ui.lang)}</span>
            <span class="chip neutral">{t(`req.event.${ev.kind}` as 'req.event.created')}</span>
            {#if ev.actor}<span class="mono">{ev.actor}</span>{/if}
            {#if ev.note}<span class="note"><Vis text={ev.note} /></span>{/if}
            {#if ev.message_id && r.thread}<a href={href(['thread', r.thread], { m: ev.message_id })}>#{ev.message_id}</a>{/if}
          </li>
        {/each}
      </ol>
    </section>
  {/if}
</div>

<style>
  .reqpage {
    max-width: 900px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .back {
    margin: 0;
  }
  h1 {
    overflow-wrap: anywhere;
  }
  .st-open {
    color: var(--warn);
    background: var(--warn-weak);
  }
  .st-answered,
  .st-expired {
    color: var(--ok);
  }
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 4px 14px;
    margin: 0;
    padding: 10px 14px;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 8px;
    align-items: baseline;
  }
  .events h2 {
    font-size: 14px;
    margin-bottom: 6px;
  }
  .events ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .events li {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 8px;
    align-items: baseline;
  }
  .when {
    font-size: 12px;
  }
  .note {
    overflow-wrap: anywhere;
  }
</style>
