<script lang="ts">
  // The lead's report (`GET /v1/tasks/report`): one section per kind with
  // its count; the first three grouped by seat. Loaded again on every round
  // of task events.
  import Vis from './Vis.svelte';
  import { get, qs } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { stateTone, taskId, waitsList } from '../lib/tasks';
  import { taskError } from '../lib/taskops';
  import type { ReportItem, ReportSection, TaskReport } from '../lib/types';
  import { isKey } from '../lib/i18n';

  let { team }: { team: string } = $props();

  let report = $state.raw<TaskReport | null>(null);
  let error = $state<string | null>(null);
  let gen = 0;

  async function load() {
    const g = ++gen;
    try {
      const r = await get<TaskReport>(`/v1/tasks/report${qs({ team: team || undefined })}`);
      if (g !== gen) return;
      report = r;
      error = null;
    } catch (e) {
      if (g === gen) error = taskError(e);
    }
  }

  $effect(() => {
    void team;
    void store.taskSeq;
    void load();
  });

  const GROUPED = ['not_started', 'in_progress', 'review'];

  function title(name: string): string {
    const k = `report.${name}`;
    return isKey(k) ? t(k) : name;
  }

  /** The items of a section by seat, in the server's order. */
  function bySeat(s: ReportSection): [string, ReportItem[]][] {
    const out: [string, ReportItem[]][] = [];
    for (const i of s.items) {
      const seat = i.seat ?? '—';
      const last = out[out.length - 1];
      if (last && last[0] === seat) last[1].push(i);
      else out.push([seat, [i]]);
    }
    return out;
  }
</script>

{#snippet row(i: ReportItem, section: string)}
  <li data-task={i.task.num}>
    <a class="mono" href={href(['tasks', taskId(i.task.num)])}>{taskId(i.task.num)}</a>
    <a href={href(['tasks', taskId(i.task.num)])}><Vis text={i.task.title} /></a>
    <span class="chip tone-{stateTone(i.task.state)}">{t(`task.state.${i.task.state}`)}</span>
    {#if !GROUPED.includes(section) && i.task.assignee}<span class="mono faint">{i.task.assignee}</span>{/if}
    {#if section === 'review' && i.seat}<span class="faint">{t('report.reviewer', { seat: i.seat })}</span>{/if}
    {#if i.task.progress}<span class="chip neutral" data-testid="progress">{i.task.progress.done}/{i.task.progress.total}</span>{/if}
    {#if i.task.result_ref}<span class="mono faint">{i.task.result_ref}</span>{/if}
    {#if i.waiting_for?.length}<span class="warn">{t('report.waitsFor', { list: i.waiting_for.join(', ') })}</span>{/if}
    {#if section === 'blocked' && i.task.waits_on?.length}<span class="faint">({waitsList(i.task, t('task.okNeeded'))})</span>{/if}
    {#if i.dependents?.length}<span class="faint">{t('report.stillOpen', { list: i.dependents.map((d) => taskId(d.num)).join(', ') })}</span>{/if}
  </li>
{/snippet}

<div class="report" data-testid="task-report">
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if !report && !error}
    <p class="faint">{t('app.loading')}</p>
  {:else if report}
    {#each report.sections as s (s.name)}
      <section class="card sec" data-section={s.name}>
        <h2>{title(s.name)} <span class="count">{s.count}</span></h2>
        {#if !s.items.length}
          <p class="faint">{t('report.none')}</p>
        {:else if GROUPED.includes(s.name)}
          {#each bySeat(s) as [seat, items] (seat)}
            <h3 class="mono">{seat}</h3>
            <ul>{#each items as i (i.task.num)}{@render row(i, s.name)}{/each}</ul>
          {/each}
        {:else}
          <ul>{#each s.items as i (i.task.num)}{@render row(i, s.name)}{/each}</ul>
        {/if}
        {#if s.count > s.items.length}<p class="faint">{t('report.more', { n: s.count - s.items.length })}</p>{/if}
      </section>
    {/each}
  {/if}
</div>

<style>
  .report {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
    gap: 12px;
    align-items: start;
  }
  .sec {
    padding: 10px 12px;
    min-width: 0;
  }
  h2 {
    margin: 0 0 6px;
    font-size: 14px;
  }
  h3 {
    margin: 8px 0 2px;
    font-size: 12px;
    color: var(--muted);
    font-weight: 600;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  li {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-wrap: wrap;
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .warn {
    color: var(--warn);
  }
  @media (max-width: 720px) {
    .report {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
