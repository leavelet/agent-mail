<script lang="ts">
  // A task in a short list: `t12 Title`, a state chip, and an optional note.
  import Vis from './Vis.svelte';
  import { href } from '../lib/router.svelte';
  import { t } from '../lib/ui.svelte';
  import { stateTone, taskId } from '../lib/tasks';
  import type { TaskState } from '../lib/types';

  let { num, title, state, note }: { num: number; title: string; state: TaskState; note?: string } = $props();
</script>

<li class="tref" data-task={num}>
  <a class="mono" href={href(['tasks', taskId(num)])}>{taskId(num)}</a>
  <a class="title" href={href(['tasks', taskId(num)])}><Vis text={title} /></a>
  <span class="chip tone-{stateTone(state)}">{t(`task.state.${state}`)}</span>
  {#if note}<span class="faint note">{note}</span>{/if}
</li>

<style>
  .tref {
    display: flex;
    gap: 6px;
    align-items: baseline;
    flex-wrap: wrap;
    min-width: 0;
  }
  .title {
    overflow-wrap: anywhere;
  }
  .note {
    font-size: 12px;
  }
</style>
