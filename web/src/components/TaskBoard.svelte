<script lang="ts">
  // The board: one column per shown state. Cards are sorted by priority,
  // rank and number; dragging a card within its column (or Alt+Up/Down on a
  // focused card) sets its rank. A card cannot be dropped in another
  // column: the state is changed on the task page.
  import TaskCard from './TaskCard.svelte';
  import { t } from '../lib/ui.svelte';
  import { rankForDrop, sortTasks, taskId, type RankWrite } from '../lib/tasks';
  import { setPriority, setRank, taskError } from '../lib/taskops';
  import type { BoardColumn, Task, TaskPriority } from '../lib/types';

  let {
    columns,
    filtered = false,
    onapply,
    onreload,
    ondragging,
  }: {
    columns: BoardColumn[];
    /** A search hides some cards: dragging is off (the neighbours would be wrong). */
    filtered?: boolean;
    /** A task as the server answered a change. */
    onapply: (task: Task) => void;
    /** Load the board again (after a failed change). */
    onreload: () => void;
    ondragging?: (on: boolean) => void;
  } = $props();

  const sorted = $derived(columns.map((c) => ({ ...c, tasks: sortTasks(c.tasks) })));
  let error = $state<string | null>(null);
  let busy = $state<Set<number>>(new Set());
  let drag = $state<{ num: number; col: string } | null>(null);
  /** Where the dragged card would land: a position in its column without it. */
  let dropAt = $state<number | null>(null);

  function mark(num: number, on: boolean) {
    const next = new Set(busy);
    if (on) next.add(num);
    else next.delete(num);
    busy = next;
  }

  async function priority(task: Task, p: TaskPriority) {
    if (p === task.priority) return;
    error = null;
    mark(task.num, true);
    try {
      onapply((await setPriority(task.num, p)).task);
    } catch (e) {
      error = taskError(e);
      onreload();
    } finally {
      mark(task.num, false);
    }
  }

  async function write(writes: RankWrite[]) {
    if (!writes.length) return;
    error = null;
    // Shown at once; the answers replace them.
    for (const w of writes) {
      const task = columns.flatMap((c) => c.tasks).find((x) => x.num === w.num);
      if (task) onapply({ ...task, rank: w.rank });
    }
    for (const w of writes) {
      mark(w.num, true);
      try {
        onapply((await setRank(w.num, w.rank)).task);
      } catch (e) {
        error = taskError(e);
        onreload();
        return;
      } finally {
        mark(w.num, false);
      }
    }
  }

  function start(e: DragEvent, num: number, col: string) {
    if (filtered) {
      e.preventDefault();
      return;
    }
    drag = { num, col };
    dropAt = null;
    ondragging?.(true);
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData('text/plain', taskId(num));
    }
  }

  function end() {
    drag = null;
    dropAt = null;
    ondragging?.(false);
  }

  /** The drop position for the pointer over column `col`'s card list. */
  function over(e: DragEvent, col: BoardColumn) {
    if (!drag || drag.col !== col.state) return; // no drop target: another column
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    const others = col.tasks.filter((x) => x.num !== drag!.num);
    const list = e.currentTarget as HTMLElement;
    const cards = [...list.querySelectorAll<HTMLElement>('[data-slot]')].filter((el) => Number(el.dataset.slot) !== drag!.num);
    let i = others.length;
    for (let k = 0; k < cards.length; k++) {
      const r = cards[k].getBoundingClientRect();
      if (e.clientY < r.top + r.height / 2) {
        i = k;
        break;
      }
    }
    dropAt = i;
  }

  function drop(e: DragEvent, col: BoardColumn) {
    if (!drag || drag.col !== col.state) return;
    e.preventDefault();
    const num = drag.num;
    const at = dropAt ?? col.tasks.length;
    end();
    void write(rankForDrop(col.tasks, num, at));
  }

  function key(e: KeyboardEvent, task: Task, col: BoardColumn) {
    if (!e.altKey || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') || filtered) return;
    e.preventDefault();
    const k = col.tasks.findIndex((x) => x.num === task.num);
    void write(rankForDrop(col.tasks, task.num, e.key === 'ArrowUp' ? k - 1 : k + 1));
  }
</script>

{#if error}<p class="error" role="alert">{error}</p>{/if}
<div class="board" data-testid="task-board">
  {#each sorted as col (col.state)}
    <section class="col" class:refuse={drag && drag.col !== col.state} data-column={col.state} aria-label={t(`task.state.${col.state}`)}>
      <header>
        <span class="name st-{col.state}" title={t(`task.state.${col.state}.long`)}>{t(`task.state.${col.state}`)}</span>
        <span class="count" data-testid="column-count">{col.count}</span>
      </header>
      <div class="cards" role="list" ondragover={(e) => over(e, col)} ondrop={(e) => drop(e, col)}>
        {#if col.tasks.length === 0}<div class="none faint">{t('task.column.empty')}</div>{/if}
        {#each col.tasks as task, i (task.num)}
          {@const othersIndex = drag && drag.col === col.state ? col.tasks.slice(0, i).filter((x) => x.num !== drag!.num).length : i}
          {#if drag && drag.col === col.state && dropAt === othersIndex && task.num !== drag.num}<div class="mark" aria-hidden="true"></div>{/if}
          <!-- The card is focusable so that Alt+Up/Down can move it (the
               keyboard way to drag); its links and control stay their own. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
          <div
            class="slot"
            class:dragging={drag?.num === task.num}
            role="listitem"
            data-slot={task.num}
            draggable={!filtered}
            tabindex="0"
            title={filtered ? undefined : t('task.dragHint')}
            ondragstart={(e) => start(e, task.num, col.state)}
            ondragend={end}
            onkeydown={(e) => key(e, task, col)}
          >
            <TaskCard {task} busy={busy.has(task.num)} onpriority={(p) => priority(task, p)} />
          </div>
        {/each}
        {#if drag && drag.col === col.state && dropAt === col.tasks.filter((x) => x.num !== drag!.num).length}<div class="mark" aria-hidden="true"></div>{/if}
        {#if col.count > col.tasks.length}<div class="more faint">{t('task.column.more', { n: col.count - col.tasks.length })}</div>{/if}
      </div>
    </section>
  {/each}
</div>

<style>
  .board {
    display: flex;
    gap: 10px;
    overflow-x: auto;
    padding-bottom: 8px;
    align-items: flex-start;
    max-width: 100%;
  }
  .col {
    flex: 0 0 248px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    display: flex;
    flex-direction: column;
    max-height: calc(100vh - 170px);
    min-height: 120px;
  }
  .col.refuse {
    opacity: 0.55;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
  }
  .name {
    font-weight: 600;
  }
  .st-ready {
    color: var(--ok);
  }
  .st-doing {
    color: var(--accent);
  }
  .st-blocked {
    color: var(--danger);
  }
  .st-review {
    color: var(--notice);
  }
  .st-open {
    color: var(--warn);
  }
  .st-done,
  .st-cancelled {
    color: var(--muted);
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 6px;
    overflow-y: auto;
    flex: 1;
    min-height: 60px;
  }
  .slot[draggable='true'] {
    cursor: grab;
  }
  .slot.dragging {
    opacity: 0.4;
  }
  .slot:focus-visible {
    outline: 2px solid var(--focus);
    border-radius: var(--radius);
  }
  .mark {
    height: 3px;
    background: var(--accent);
    border-radius: 2px;
  }
  .none,
  .more {
    font-size: 12px;
    padding: 4px 2px;
  }
  @media (max-width: 720px) {
    .col {
      flex-basis: 78vw;
      max-height: none;
    }
  }
</style>
