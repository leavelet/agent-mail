<script lang="ts">
  // One task: the record, its result, its dependencies, children, linked
  // threads with their latest message, the discussion thread and the
  // history, with the user's actions (state, result, assignee, priority,
  // due, labels, dependencies, links, comments). Changes made elsewhere are
  // shown live; an edit of the title or body is refused when the task
  // changed since it was loaded.
  import Vis from '../components/Vis.svelte';
  import MessageCard from '../components/MessageCard.svelte';
  import TaskHistory from '../components/TaskHistory.svelte';
  import { del, get, post } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { go, href } from '../lib/router.svelte';
  import { shortSubject } from '../lib/subject';
  import { fullTime, shortTime } from '../lib/time';
  import {
    PRIORITIES,
    concerns,
    isoToLocal,
    localToIso,
    nextStates,
    openChildren,
    overdue,
    parseLabels,
    parseTaskNum,
    stateTone,
    taskId,
    waitsList,
  } from '../lib/tasks';
  import { addDep, getTask, patchTask, removeDep, taskError, taskPath } from '../lib/taskops';
  import { requestId } from '../lib/requests';
  import type { Message, StoredTaskState, TaskCommentResponse, TaskDetail, TaskPriority, TaskRef, TaskResult, ThreadResponse } from '../lib/types';

  let { num }: { num: number } = $props();

  let detail = $state.raw<TaskDetail | null>(null);
  let loadError = $state<string | null>(null);
  let error = $state<string | null>(null);
  let note = $state<string | null>(null);
  let busy = $state(false);
  let gen = 0;

  async function load() {
    const g = ++gen;
    try {
      const d = await getTask(num);
      if (g !== gen) return;
      detail = d;
      loadError = null;
    } catch (e) {
      if (g === gen) loadError = taskError(e);
    }
  }

  $effect(() => {
    void num;
    detail = null;
    loadError = null;
    error = null;
    note = null;
    resetDrafts();
    void load();
  });

  // Live: reload when this task or one it names changed.
  let seenSeq = store.taskSeq;
  $effect(() => {
    const seq = store.taskSeq;
    if (seq === seenSeq) return;
    seenSeq = seq;
    const d = detail;
    const shown = d ? [num, ...d.blockers.map((x) => x.num), ...d.blocked.map((x) => x.num), ...d.children.map((x) => x.num)] : [num];
    if (concerns(store.taskChanged, shown)) void load();
  });

  /** Run a change; its answer is the new record. */
  async function act(f: () => Promise<TaskDetail>) {
    busy = true;
    error = null;
    note = null;
    try {
      detail = await f();
      return true;
    } catch (e) {
      error = taskError(e);
      return false;
    } finally {
      busy = false;
    }
  }

  const task = $derived(detail?.task);
  const seats = $derived(store.seats.map((s) => s.name));

  // Drafts of the inputs: null while the person has not changed them (the
  // input then shows the record, also after a live update).
  let stateDraft = $state<StoredTaskState | ''>('');
  let waitingDraft = $state('');
  let reasonDraft = $state('');
  let dueDraft = $state<string | null>(null);
  let labelsDraft = $state<string | null>(null);
  let depDraft = $state('');
  let linkDraft = $state('');
  let linkKind = $state<'thread' | 'message'>('thread');
  let comment = $state('');
  let editing = $state(false);
  let titleDraft = $state('');
  let bodyDraft = $state('');
  let editRev = $state<number | undefined>(undefined);
  let forceDraft = $state(false);
  let depNeedsOk = $state(false);
  let resultDraft = $state<TaskResult>('ok');
  let refDraft = $state('');
  let resultNote = $state('');
  let withdrawReason = $state('');

  function resetDrafts() {
    stateDraft = '';
    waitingDraft = '';
    reasonDraft = '';
    dueDraft = null;
    labelsDraft = null;
    depDraft = '';
    linkDraft = '';
    comment = '';
    editing = false;
    forceDraft = false;
    depNeedsOk = false;
    resultDraft = 'ok';
    refDraft = '';
    resultNote = '';
    withdrawReason = '';
  }

  const openKids = $derived(detail ? openChildren(detail.children) : []);
  const closing = $derived(stateDraft === 'done' || stateDraft === 'cancelled');

  async function setResult(e: SubmitEvent) {
    e.preventDefault();
    const body: Record<string, unknown> = { result: resultDraft };
    if (refDraft.trim()) body.result_ref = refDraft.trim();
    if (resultNote.trim()) body.result_note = resultNote.trim();
    if (await act(() => patchTask(num, body))) {
      refDraft = '';
      resultNote = '';
    }
  }

  async function withdrawResult(e: SubmitEvent) {
    e.preventDefault();
    const body: Record<string, unknown> = { result: null };
    if (withdrawReason.trim()) body.note = withdrawReason.trim();
    if (await act(() => patchTask(num, body))) withdrawReason = '';
  }

  async function setState(e: SubmitEvent) {
    e.preventDefault();
    if (!stateDraft) return;
    const body: Record<string, unknown> = { state: stateDraft };
    if (stateDraft === 'blocked') {
      body.waiting_on = waitingDraft || null;
      body.blocked_reason = reasonDraft.trim() || null;
    }
    if (closing && forceDraft) body.force = true;
    if (await act(() => patchTask(num, body))) {
      stateDraft = '';
      waitingDraft = '';
      reasonDraft = '';
      forceDraft = false;
    }
  }

  function setAssignee(v: string) {
    void act(() => patchTask(num, { assignee: v || null }));
  }

  function setPriority(v: TaskPriority) {
    void act(() => patchTask(num, { priority: v }));
  }

  async function saveDue(clear = false) {
    const v = clear ? null : localToIso(dueDraft ?? '') || null;
    if (await act(() => patchTask(num, { due: v }))) dueDraft = null;
  }

  async function saveLabels(e: SubmitEvent) {
    e.preventDefault();
    if (await act(() => patchTask(num, { labels: parseLabels(labelsDraft ?? '') }))) labelsDraft = null;
  }

  async function addDependency(e: SubmitEvent) {
    e.preventDefault();
    const other = parseTaskNum(depDraft);
    if (!other) {
      error = t('task.err.badNumber');
      return;
    }
    if (await act(() => addDep(num, other, depNeedsOk || undefined))) {
      depDraft = '';
      depNeedsOk = false;
    }
  }

  async function link(e: SubmitEvent) {
    e.preventDefault();
    const id = Number(linkDraft.trim().replace(/^#/, ''));
    if (!Number.isInteger(id) || id <= 0) {
      error = t('task.err.badId');
      return;
    }
    const body = linkKind === 'thread' ? { thread_id: id } : { message_id: id };
    if (await act(() => post<TaskDetail>(taskPath(num, '/links'), body))) linkDraft = '';
  }

  async function sendComment(e: SubmitEvent) {
    e.preventDefault();
    const text = comment.trim();
    if (!text) return;
    busy = true;
    error = null;
    note = null;
    try {
      const sent = await post<TaskCommentResponse>(taskPath(num, '/comments'), { body: text });
      comment = '';
      note = t('task.commented', { id: sent.id });
      discussionVersion++;
      await load();
      void store.mailChanged();
    } catch (err) {
      error = taskError(err);
    } finally {
      busy = false;
    }
  }

  function startEdit() {
    if (!task) return;
    titleDraft = task.title;
    bodyDraft = task.body ?? '';
    editRev = task.rev;
    editing = true;
  }

  async function saveEdit(e: SubmitEvent) {
    e.preventDefault();
    if (!task) return;
    const body: Record<string, unknown> = {};
    if (titleDraft.trim() !== task.title) body.title = titleDraft.trim();
    if (bodyDraft !== (task.body ?? '')) body.body = bodyDraft;
    if (!Object.keys(body).length) {
      editing = false;
      return;
    }
    // Refused (`stale`) when someone changed the task since the edit began.
    if (editRev !== undefined) body.if_rev = editRev;
    if (await act(() => patchTask(num, body))) editing = false;
    else editRev = detail?.task.rev;
  }

  // Linked threads with their latest message (loaded again when the links change).
  let latest = $state.raw<Map<number, Message | null>>(new Map());
  const linkKey = $derived(detail ? detail.links.map((l) => l.thread_id).join(',') : '');
  $effect(() => {
    const ids = linkKey ? linkKey.split(',').map(Number) : [];
    const next = new Map<number, Message | null>();
    void Promise.all(
      ids.map((id) =>
        get<ThreadResponse>(`/v1/threads/${id}`)
          .then((r) => next.set(id, r.messages[r.messages.length - 1] ?? null))
          .catch(() => next.set(id, null)),
      ),
    ).then(() => {
      if (ids.join(',') === linkKey) latest = next;
    });
  });

  // The discussion thread: loaded again when the task has new events
  // (a comment adds one) or after the user commented.
  let discussion = $state.raw<ThreadResponse | null>(null);
  let discussionError = $state<string | null>(null);
  let discussionVersion = $state(0);
  const threadId = $derived(task?.thread);
  const eventCount = $derived(detail?.event_count ?? 0);
  $effect(() => {
    const id = threadId;
    void eventCount;
    void discussionVersion;
    if (!id) {
      discussion = null;
      return;
    }
    get<ThreadResponse>(`/v1/threads/${id}`)
      .then((r) => {
        if (threadId === id) {
          discussion = r;
          discussionError = null;
        }
      })
      .catch((e) => (discussionError = taskError(e)));
  });

  const refList = (label: string, list: TaskRef[], removable = false, children = false) => ({ label, list, removable, children });
  const groups = $derived(
    detail
      ? [refList(t('task.blockers'), detail.blockers, true), refList(t('task.blocked'), detail.blocked), refList(t('task.children'), detail.children, false, true)]
      : [],
  );
</script>

<div class="page taskpage">
  <p class="back"><a href={href(['tasks'])}>← {t('tasks.title')}</a></p>
  {#if loadError}
    <p class="error" role="alert">{t('task.notFound', { id: taskId(num) })} ({loadError})</p>
  {:else if !detail || !task}
    <p class="faint">{t('app.loading')}</p>
  {:else}
    <div class="page-head">
      <h1><span class="mono faint">{taskId(task.num)}</span> <Vis text={task.title} /></h1>
      <span class="chip tone-{stateTone(task.state)}" data-testid="task-state">{t(`task.state.${task.state}`)}</span>
      {#if task.priority !== 'normal'}<span class="chip {task.priority === 'urgent' ? 'urgent' : 'neutral'}">{t(`task.priority.${task.priority}`)}</span>{/if}
      <span class="spacer"></span>
      {#if !editing}<button class="small" onclick={startEdit}>{t('task.edit')}</button>{/if}
    </div>
    {#if task.steal_requested_by}
      <div class="stealbox card" data-testid="task-steal">
        <span>{t('task.steal.asks', { seat: task.steal_requested_by })} · <span title={fullTime(task.steal_requested_at, ui.lang)}>{shortTime(task.steal_requested_at, ui.now, ui.lang)}</span></span>
        <span class="spacer"></span>
        <button class="small primary" disabled={busy} onclick={() => act(() => post<TaskDetail>(taskPath(num, '/yield')))}>{t('task.steal.yield', { seat: task.steal_requested_by })}</button>
        <button class="small" disabled={busy} onclick={() => act(() => post<TaskDetail>(taskPath(num, '/keep')))}>{t('task.steal.keep', { seat: task.assignee ?? '—' })}</button>
      </div>
    {/if}
    {#if error}<p class="error" role="alert" data-testid="task-error">{error}</p>{/if}
    {#if note}<p class="ok" role="status">{note}</p>{/if}

    <div class="cols">
      <div class="mainc">
        {#if editing}
          <form class="card edit" onsubmit={saveEdit}>
            <label>{t('task.title')}<input bind:value={titleDraft} maxlength="512" /></label>
            <label>{t('task.body')}<textarea rows="8" bind:value={bodyDraft}></textarea></label>
            <div class="row">
              <button class="primary small" type="submit" disabled={busy}>{t('admin.save')}</button>
              <button class="small" type="button" onclick={() => (editing = false)}>{t('admin.cancel')}</button>
            </div>
          </form>
        {:else if task.body}
          <pre class="card body"><Vis text={task.body} /></pre>
        {:else}
          <p class="faint">{t('task.noBody')}</p>
        {/if}

        <section class="result card" data-testid="task-result">
          <h2>{t('task.result')}</h2>
          {#if task.result}
            <p class="row wrap">
              <span class="chip {task.result === 'ok' ? 'ok' : 'tone-danger'}" data-testid="result-value">{t(`task.result.${task.result}`)}</span>
              {#if task.result_ref}<span class="mono" data-testid="result-ref"><Vis text={task.result_ref} /></span>{/if}
              {#if task.result_by}<span class="faint" title={fullTime(task.result_at, ui.lang)}>{t('task.result.by', { seat: task.result_by, time: shortTime(task.result_at, ui.now, ui.lang) })}</span>{/if}
            </p>
            {#if task.result_note}<p class="rnote"><Vis text={task.result_note} /></p>{/if}
          {:else}
            <p class="faint">{t('task.result.none')}</p>
          {/if}
          {#if task.stored_state === 'done'}
            <form class="row wrap" onsubmit={setResult} data-testid="result-form">
              <select bind:value={resultDraft} aria-label={t('task.result')}>
                <option value="ok">{t('task.result.ok')}</option>
                <option value="rejected">{t('task.result.rejected')}</option>
              </select>
              <input class="mono" placeholder={t('task.result.ref')} aria-label={t('task.result.ref')} maxlength="200" bind:value={refDraft} />
              <input placeholder={t('task.result.note')} aria-label={t('task.result.note')} maxlength="2000" bind:value={resultNote} />
              <button class="small" type="submit" disabled={busy}>{t('task.result.set')}</button>
            </form>
            {#if task.result}
              <form class="row wrap" onsubmit={withdrawResult} data-testid="result-withdraw">
                <input placeholder={t('task.result.reason')} aria-label={t('task.result.reason')} maxlength="2000" bind:value={withdrawReason} />
                <button class="small" type="submit" disabled={busy}>{t('task.result.withdraw')}</button>
              </form>
            {/if}
          {:else}
            <p class="faint hint">{t('task.result.onlyDone')}</p>
          {/if}
        </section>

        {#if task.state === 'open' && task.waits_on?.length}
          <p class="waits">{t('task.waitsOn', { list: waitsList(task, t('task.okNeeded')) })}</p>
        {/if}
        {#if task.blocked_on_human}
          <p class="waits blocked" data-testid="task-blocked-on-human">
            {t('task.blockedOnHuman')} (<a class="mono" href={href(['requests', requestId(task.blocked_on_human)])}>{requestId(task.blocked_on_human)}</a>)
          </p>
        {/if}
        {#if task.stored_state === 'blocked'}
          <p class="waits blocked" data-testid="task-blocked">
            {#if task.waiting_on}{t('task.waitsFor', { seat: task.waiting_on })}{:else}{t('task.blockedNoSeat')}{/if}
            {#if task.blocked_reason}· <Vis text={task.blocked_reason} />{/if}
          </p>
        {/if}

        {#each groups as g}
          <section class="refs">
            <h2>{g.label} <span class="count">{g.list.length}</span>
              {#if g.children && task.progress}<span class="faint progress" data-testid="task-progress">{t('task.progress', { done: task.progress.done, total: task.progress.total })}</span>{/if}
            </h2>
            {#if g.removable}
              <form class="row wrap" onsubmit={addDependency}>
                <input class="mono short" placeholder="t9" aria-label={t('task.addBlocker')} bind:value={depDraft} />
                <label class="check"><input type="checkbox" bind:checked={depNeedsOk} />{t('task.needsOk')}</label>
                <button class="small" type="submit" disabled={busy}>{t('task.addBlocker')}</button>
              </form>
            {/if}
            {#if g.list.length}
              <ul>
                {#each g.list as r (r.num)}
                  <li data-ref={r.num}>
                    <a class="mono" href={href(['tasks', taskId(r.num)])}>{taskId(r.num)}</a>
                    <a href={href(['tasks', taskId(r.num)])}><Vis text={r.title} /></a>
                    <span class="chip tone-{stateTone(r.state)}">{t(`task.state.${r.state}`)}</span>
                    {#if r.assignee}<span class="mono faint">{r.assignee}</span>{/if}
                    {#if r.result}<span class="chip {r.result === 'ok' ? 'ok' : 'tone-danger'}">{t(`task.result.${r.result}`)}</span>{/if}
                    {#if r.needs_ok}<span class="chip neutral" data-testid="needs-ok">{t('task.okNeeded')}</span>{/if}
                    {#if g.removable}
                      <button class="small ghost" disabled={busy} title={t('task.removeBlocker')} aria-label={t('task.removeBlocker')} onclick={() => act(() => removeDep(num, r.num))}>✕</button>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </section>
        {/each}

        <section class="refs">
          <h2>{t('task.links')} <span class="count">{detail.links.length}</span></h2>
          <form class="row wrap" onsubmit={link}>
            <select bind:value={linkKind} aria-label={t('task.linkKind')}>
              <option value="thread">{t('task.linkThread')}</option>
              <option value="message">{t('task.linkMessage')}</option>
            </select>
            <input class="mono short" placeholder="123" aria-label={t('task.linkId')} bind:value={linkDraft} />
            <button class="small" type="submit" disabled={busy}>{t('task.link')}</button>
          </form>
          {#if detail.links.length}
            <ul>
              {#each detail.links as l (l.thread_id)}
                {@const m = latest.get(l.thread_id)}
                <li data-link={l.thread_id}>
                  <a class="mono" href={href(['thread', l.thread_id])}>#{l.thread_id}</a>
                  {#if m}
                    <a href={href(['thread', l.thread_id], { m: m.id })}>{#if m.subject}<Vis text={shortSubject(m.subject)} />{:else}{t('mail.noSubject')}{/if}</a>
                    <span class="faint">{t('task.latest', { from: m.from, time: shortTime(m.created_at, ui.now, ui.lang) })}</span>
                  {:else if latest.has(l.thread_id)}
                    <span class="faint">{t('task.threadGone')}</span>
                  {/if}
                  <button class="small ghost" disabled={busy} title={t('task.unlink')} aria-label={t('task.unlink')} onclick={() => act(() => del<TaskDetail>(taskPath(num, `/links/${l.thread_id}`)))}>✕</button>
                </li>
              {/each}
            </ul>
          {/if}
        </section>

        <section class="discussion">
          <h2 class="row">{t('task.discussion')}{#if task.thread}<a class="sublink" href={href(['thread', task.thread])}>{t('task.openThread')}</a>{/if}</h2>
          {#if discussionError}<p class="error">{discussionError}</p>{/if}
          {#if !task.thread}
            <p class="faint">{t('task.noDiscussion')}</p>
          {:else if discussion}
            <div class="msgs">
              {#each discussion.messages as m (m.id)}
                <MessageCard message={m}>
                  {#snippet actions()}
                    <button class="small" onclick={() => go(['compose'], { reply: m.id })}>{t('mail.reply')}</button>
                  {/snippet}
                </MessageCard>
              {/each}
            </div>
          {/if}
          <form class="comment" onsubmit={sendComment}>
            <textarea rows="3" placeholder={t('task.commentPlaceholder')} aria-label={t('task.comment')} bind:value={comment}></textarea>
            <div class="row">
              <button class="primary small" type="submit" disabled={busy || !comment.trim()}>{t('task.comment')}</button>
              <span class="faint hint">{t('task.commentHint')}</span>
            </div>
          </form>
        </section>

        <TaskHistory num={task.num} version={detail.event_count} thread={task.thread} />
      </div>

      <aside class="side card">
        <dl>
          <dt>{t('task.state')}</dt>
          <dd>
            <form class="stack" onsubmit={setState} data-testid="task-state-form">
              <div class="row wrap">
                <select bind:value={stateDraft} aria-label={t('task.setState')}>
                  <option value="">{t(`task.state.${task.state}`)}</option>
                  {#each nextStates(task.stored_state) as s}<option value={s}>{t(`task.state.${s}`)}</option>{/each}
                </select>
                <button class="small" type="submit" disabled={busy || !stateDraft}>{t('task.setState')}</button>
              </div>
              {#if stateDraft === 'blocked'}
                <label>{t('task.waitingOn')}
                  <select bind:value={waitingDraft}>
                    <option value="">{t('task.nobody')}</option>
                    {#each seats as s}<option value={s}>{s}</option>{/each}
                  </select>
                </label>
                <label>{t('task.reason')}<input maxlength="512" bind:value={reasonDraft} /></label>
              {/if}
              {#if stateDraft === 'open'}
                <span class="faint hint">{t('task.readyHint')}</span>
              {/if}
              {#if closing && openKids.length}
                <label class="check"><input type="checkbox" bind:checked={forceDraft} data-testid="force" />{t('task.force')} ({openKids.map((c) => taskId(c.num)).join(', ')})</label>
              {/if}
            </form>
          </dd>
          <dt>{t('task.priority')}</dt>
          <dd>
            <select value={task.priority} disabled={busy} aria-label={t('task.priority')} onchange={(e) => setPriority((e.currentTarget as HTMLSelectElement).value as TaskPriority)}>
              {#each PRIORITIES as p}<option value={p}>{t(`task.priority.${p}`)}</option>{/each}
            </select>
            <span class="faint">{t('task.rank')} {task.rank}</span>
          </dd>
          <dt>{t('task.assignee')}</dt>
          <dd>
            <select value={task.assignee ?? ''} disabled={busy} aria-label={t('task.assignee')} onchange={(e) => setAssignee((e.currentTarget as HTMLSelectElement).value)}>
              <option value="">{t('task.unassigned')}</option>
              {#each seats as s}<option value={s}>{s}</option>{/each}
              {#if task.assignee && !seats.includes(task.assignee)}<option value={task.assignee}>{task.assignee}</option>{/if}
            </select>
          </dd>
          <dt>{t('task.reporter')}</dt>
          <dd><a class="mono" href={href(['org', task.reporter])}>{task.reporter}</a></dd>
          <dt>{t('task.team')}</dt>
          <dd>{#if task.team}<a class="mono" href={href(['teams', task.team])}>{task.team}</a>{:else}<span class="faint">—</span>{/if}</dd>
          <dt>{t('task.parent')}</dt>
          <dd>{#if task.parent}<a class="mono" href={href(['tasks', taskId(task.parent)])}>{taskId(task.parent)}</a>{:else}<span class="faint">—</span>{/if}</dd>
          <dt>{t('task.due')}</dt>
          <dd>
            {#if task.due}<div class:late={overdue(task, ui.now)} title={fullTime(task.due, ui.lang)}>{shortTime(task.due, ui.now, ui.lang)}</div>{/if}
            <div class="row wrap">
              <input type="datetime-local" aria-label={t('task.due')} value={dueDraft ?? isoToLocal(task.due)} oninput={(e) => (dueDraft = (e.currentTarget as HTMLInputElement).value)} />
              <button class="small" disabled={busy || dueDraft === null} onclick={() => saveDue()}>{t('admin.save')}</button>
              {#if task.due}<button class="small ghost" disabled={busy} onclick={() => saveDue(true)}>{t('task.clear')}</button>{/if}
            </div>
          </dd>
          <dt>{t('task.labels')}</dt>
          <dd>
            <form class="row wrap" onsubmit={saveLabels}>
              <input placeholder={t('task.labelsHint')} aria-label={t('task.labels')} value={labelsDraft ?? (task.labels ?? []).join(', ')} oninput={(e) => (labelsDraft = (e.currentTarget as HTMLInputElement).value)} />
              <button class="small" type="submit" disabled={busy || labelsDraft === null}>{t('admin.save')}</button>
            </form>
          </dd>
          <dt>{t('task.created')}</dt>
          <dd title={fullTime(task.created_at, ui.lang)}>{shortTime(task.created_at, ui.now, ui.lang)}</dd>
          <dt>{t('task.updated')}</dt>
          <dd title={fullTime(task.updated_at, ui.lang)}>{shortTime(task.updated_at, ui.now, ui.lang)}</dd>
          {#if task.closed_at}
            <dt>{t('task.closed')}</dt>
            <dd title={fullTime(task.closed_at, ui.lang)}>{shortTime(task.closed_at, ui.now, ui.lang)}</dd>
          {/if}
          {#if task.import_key}
            <dt>{t('task.importedAs')}</dt>
            <dd class="mono"><Vis text={task.import_key} /></dd>
          {/if}
          <dt>{t('task.revision')}</dt>
          <dd class="mono faint">{task.rev ?? 0}</dd>
        </dl>
      </aside>
    </div>
  {/if}
</div>

<style>
  .taskpage {
    max-width: 1200px;
  }
  .back {
    margin: 0 0 6px;
    font-size: 12px;
  }
  h1 {
    overflow-wrap: anywhere;
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: 16px;
    align-items: start;
  }
  .mainc {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-width: 0;
  }
  .body {
    padding: 10px 12px;
  }
  .edit {
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .edit label,
  .stack label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  .waits {
    margin: 0;
    color: var(--warn);
  }
  .waits.blocked {
    color: var(--danger);
  }
  .refs ul {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .refs li {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-wrap: wrap;
  }
  .refs form {
    margin-top: 6px;
  }
  .short {
    width: 90px;
  }
  .result {
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .result h2 {
    margin: 0;
  }
  .result p {
    margin: 0;
  }
  .rnote {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .progress {
    font-size: 12px;
    font-weight: 400;
  }
  label.check {
    display: flex;
    flex-direction: row;
    align-items: center;
    gap: 4px;
    font-size: 12px;
    color: var(--muted);
  }
  .stealbox {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
    padding: 8px 12px;
    margin-bottom: 10px;
    background: var(--notice-weak);
    color: var(--notice);
    font-weight: 600;
  }
  .sublink {
    font-size: 12px;
    font-weight: 400;
  }
  .msgs {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 6px 0;
  }
  .comment {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 6px;
  }
  .hint {
    font-size: 12px;
  }
  .side {
    padding: 10px 12px;
  }
  dl {
    margin: 0;
    display: grid;
    grid-template-columns: 1fr;
    gap: 2px;
  }
  dt {
    font-size: 12px;
    color: var(--muted);
    margin-top: 8px;
  }
  dt:first-child {
    margin-top: 0;
  }
  dd {
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    align-items: flex-start;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .late {
    color: var(--danger);
    font-weight: 600;
  }
  @media (max-width: 900px) {
    .cols {
      grid-template-columns: minmax(0, 1fr);
    }
    .side {
      order: -1;
    }
  }
  @media (max-width: 720px) {
    .taskpage {
      padding: 12px 16px;
    }
  }
</style>
