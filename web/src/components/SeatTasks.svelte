<script lang="ts">
  // A seat's tasks next to its mail: its unfinished tasks (`mine`) and
  // what other seats wait for from it (`owed`). For the user seat these are
  // `GET /v1/tasks/mine` and `/owed`; those calls answer for the caller
  // only, so for another seat the same lists are computed from listings.
  import { get } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { owedFor, personalOrder, taskId, type OwedEntry } from '../lib/tasks';
  import { listTasks, taskError } from '../lib/taskops';
  import type { OwedList, Task, TaskList } from '../lib/types';
  import TaskRefRow from './TaskRefRow.svelte';

  let { seat }: { seat: string } = $props();

  const SHOWN = 10;
  let mine = $state.raw<Task[]>([]);
  let owed = $state.raw<OwedEntry[]>([]);
  let more = $state(false);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let gen = 0;

  async function load(name: string) {
    const g = ++gen;
    try {
      let m: Task[];
      let o: OwedEntry[];
      let truncated = false;
      if (name === store.me?.seat) {
        const [a, b] = await Promise.all([get<TaskList>('/v1/tasks/mine'), get<OwedList>('/v1/tasks/owed')]);
        m = a.tasks;
        o = b.items;
        truncated = !!a.truncated || !!b.truncated;
      } else {
        const [a, w, r] = await Promise.all([
          listTasks({ assignee: name, limit: 500 }),
          listTasks({ state: 'open,blocked', limit: 500 }),
          listTasks({ reporter: name, assignee: 'none', limit: 500 }),
        ]);
        m = personalOrder(a.tasks);
        o = owedFor(name, w.tasks, [...a.tasks, ...r.tasks]);
        truncated = !!a.next_before || !!w.next_before;
      }
      if (g !== gen) return;
      mine = m;
      owed = o;
      more = truncated;
      error = null;
      loaded = true;
    } catch (e) {
      if (g === gen) error = taskError(e);
    }
  }

  $effect(() => {
    const name = seat;
    void store.taskSeq;
    void load(name);
  });

  function owedNote(o: OwedEntry): string {
    if (o.blocks) return t('task.owed.blocks', { id: taskId(o.blocks.num), seat: o.seat ?? '' });
    return t('task.owed.waiting', { seat: o.seat ?? '' });
  }
</script>

<div class="seattasks" data-testid="seat-tasks">
  {#if error}<p class="error">{error}</p>{/if}
  <section data-list="mine">
    <h3>{t('task.mine')} <span class="count">{mine.length}</span></h3>
    {#if loaded && mine.length === 0}<p class="faint none">{t('task.mine.none')}</p>{/if}
    <ul>
      {#each mine.slice(0, SHOWN) as x (x.num)}
        <TaskRefRow num={x.num} title={x.title} state={x.state} note={x.priority === 'normal' ? undefined : t(`task.priority.${x.priority}`)} />
      {/each}
    </ul>
    {#if mine.length > SHOWN}<a class="small" href={href(['tasks'])}>{t('task.moreTasks', { n: mine.length - SHOWN })}</a>{/if}
  </section>
  <section data-list="owed">
    <h3 title={t('task.owed.long')}>{t('task.owed')} <span class="count {owed.length ? 'alert' : ''}">{owed.length}</span></h3>
    {#if loaded && owed.length === 0}<p class="faint none">{t('task.owed.none')}</p>{/if}
    <ul>
      {#each owed.slice(0, SHOWN) as o (`${o.task.num}-${o.blocks?.num ?? ''}`)}
        <TaskRefRow num={o.task.num} title={o.task.title} state={o.task.state} note={owedNote(o)} />
      {/each}
    </ul>
  </section>
  {#if more}<p class="faint none">{t('task.truncated')}</p>{/if}
</div>

<style>
  .seattasks {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 6px 16px;
  }
  h3 {
    font-size: 12px;
    color: var(--muted);
    margin-bottom: 4px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .none {
    margin: 0;
    font-size: 12px;
  }
  .small {
    font-size: 12px;
  }
</style>
