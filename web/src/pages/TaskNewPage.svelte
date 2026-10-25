<script lang="ts">
  // Create a task (`POST /v1/tasks`) as the user seat.
  import { post } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { go, href } from '../lib/router.svelte';
  import { PRIORITIES, localToIso, parseLabels, parseTaskNum, taskId } from '../lib/tasks';
  import { taskError } from '../lib/taskops';
  import type { TaskDetail, TaskPriority } from '../lib/types';

  let title = $state('');
  let body = $state('');
  let priority = $state<TaskPriority>('normal');
  let assignee = $state('');
  let parent = $state('');
  let labels = $state('');
  let due = $state('');
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    error = null;
    const p = parent.trim() ? parseTaskNum(parent) : null;
    if (parent.trim() && !p) {
      error = t('task.err.badNumber');
      return;
    }
    busy = true;
    try {
      const d = await post<TaskDetail>('/v1/tasks', {
        title: title.trim(),
        body: body || undefined,
        priority,
        assignee: assignee || undefined,
        parent: p ?? undefined,
        labels: parseLabels(labels),
        due: due ? localToIso(due) : undefined,
      });
      go(['tasks', taskId(d.task.num)]);
    } catch (err) {
      error = taskError(err);
    } finally {
      busy = false;
    }
  }
</script>

<div class="page newtask">
  <p class="back"><a href={href(['tasks'])}>← {t('tasks.title')}</a></p>
  <div class="page-head"><h1>{t('tasks.new')}</h1></div>
  <form class="card" onsubmit={submit}>
    <label>{t('task.title')}<input id="task-title" required maxlength="512" bind:value={title} /></label>
    <label>{t('task.body')}<textarea rows="6" bind:value={body}></textarea></label>
    <div class="row wrap">
      <label>{t('task.priority')}
        <select bind:value={priority}>{#each PRIORITIES as p}<option value={p}>{t(`task.priority.${p}`)}</option>{/each}</select>
      </label>
      <label>{t('task.assignee')}
        <select bind:value={assignee}>
          <option value="">{t('task.unassigned')}</option>
          {#each store.seats as s}<option value={s.name}>{s.name}</option>{/each}
        </select>
      </label>
      <label>{t('task.parent')}<input class="mono short" placeholder="t12" bind:value={parent} /></label>
      <label>{t('task.due')}<input type="datetime-local" bind:value={due} /></label>
    </div>
    <label>{t('task.labels')}<input placeholder={t('task.labelsHint')} bind:value={labels} /></label>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="row">
      <button class="primary" type="submit" disabled={busy || !title.trim()}>{t('task.create')}</button>
      <span class="faint hint">{t('task.createHint')}</span>
    </div>
  </form>
</div>

<style>
  .newtask {
    max-width: 760px;
  }
  .back {
    margin: 0 0 6px;
    font-size: 12px;
  }
  form {
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  .short {
    width: 90px;
  }
  .hint {
    font-size: 12px;
  }
  @media (max-width: 720px) {
    .newtask {
      padding: 12px 16px;
    }
  }
</style>
