<script lang="ts">
  // Tasks in "needs you": tasks `blocked` with `waiting_on` the user seat,
  // and the user's own tasks whose due time has passed.
  import { onMount } from 'svelte';
  import { get } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { shortTime } from '../lib/time';
  import { listTasks } from '../lib/taskops';
  import type { OwedList, Task } from '../lib/types';
  import TaskRefRow from './TaskRefRow.svelte';

  let waiting = $state.raw<Task[]>([]);
  let late = $state.raw<Task[]>([]);
  let tick = $state(0);
  let gen = 0;

  async function load() {
    const me = store.me?.seat;
    if (!me) return;
    const g = ++gen;
    try {
      const [o, d] = await Promise.all([get<OwedList>('/v1/tasks/owed'), listTasks({ due_before: new Date().toISOString(), assignee: me, limit: 100 })]);
      if (g !== gen) return;
      waiting = o.items.filter((i) => !i.blocks && i.task.waiting_on === me).map((i) => i.task);
      late = d.tasks;
    } catch {
      // The mail queue still works without the task lists.
    }
  }

  $effect(() => {
    void store.taskSeq;
    void tick;
    void load();
  });

  // Due times pass without any event.
  onMount(() => {
    const id = setInterval(() => tick++, 60_000);
    return () => clearInterval(id);
  });
</script>

{#if waiting.length || late.length}
  <div class="ntasks" data-testid="needs-tasks">
    {#if waiting.length}
      <h3>{t('needs.tasksWaiting')} <span class="count alert">{waiting.length}</span></h3>
      <ul>
        {#each waiting as x (x.num)}
          <TaskRefRow num={x.num} title={x.title} state={x.state} note={[x.assignee, x.blocked_reason].filter(Boolean).join(' · ')} />
        {/each}
      </ul>
    {/if}
    {#if late.length}
      <h3>{t('needs.tasksOverdue')} <span class="count alert">{late.length}</span></h3>
      <ul>
        {#each late as x (x.num)}
          <TaskRefRow num={x.num} title={x.title} state={x.state} note={t('needs.dueAt', { time: shortTime(x.due, ui.now, ui.lang) })} />
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .ntasks {
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
</style>
