<script lang="ts">
  // One document: the body rendered with section anchors, the fields, the
  // relations, links in and out, linked tasks and threads, the discussion
  // thread with its section anchors, the revisions with diffs and the
  // history, and the user's actions (verify, activate, archive, supersede,
  // pin, edit fields and body with `if_rev`, comment, split, extract,
  // merge). Changes made elsewhere are shown live.
  import Vis from '../components/Vis.svelte';
  import DocBody from '../components/DocBody.svelte';
  import DocChip from '../components/DocChip.svelte';
  import DocDiff from '../components/DocDiff.svelte';
  import DocHistory from '../components/DocHistory.svelte';
  import DocPicker from '../components/DocPicker.svelte';
  import MessageCard from '../components/MessageCard.svelte';
  import TaskRefRow from '../components/TaskRefRow.svelte';
  import { get, post } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { go, href } from '../lib/router.svelte';
  import { fullTime, shortTime } from '../lib/time';
  import { shortSubject } from '../lib/subject';
  import { concerns } from '../lib/tasks';
  import { VERIFY_INTERVALS, apiRef, docHref, docId, docStatusTone, headedSections, parseDocTarget, parseList } from '../lib/docs';
  import { docAction, docError, docPath, getDoc, lastCreated, patchDoc, staleBody, writeBody } from '../lib/docops';
  import type { DocCommentResponse, DocDetail, DocKind, DocLink, DocRef, DocStaleBody, Message, ThreadResponse } from '../lib/types';

  let { target, anchor = '' }: { target: string; anchor?: string } = $props();

  const parsed = $derived(parseDocTarget(target));
  let detail = $state.raw<DocDetail | null>(null);
  let loadError = $state<string | null>(null);
  let error = $state<string | null>(null);
  let note = $state<string | null>(null);
  let busy = $state(false);
  /** What the create found (similar documents, dangling links), once. */
  let created = $state.raw<DocDetail | null>(null);
  let gen = 0;

  async function load() {
    const g = ++gen;
    try {
      const d = await getDoc(apiRef(parsed));
      if (g !== gen) return;
      detail = d;
      loadError = null;
    } catch (e) {
      if (g === gen) loadError = docError(e);
    }
  }

  $effect(() => {
    void parsed.ref;
    detail = null;
    loadError = null;
    error = null;
    note = null;
    conflict = null;
    editing = false;
    editingFields = false;
    created = lastCreated.detail && lastCreated.detail.doc.num === parsed.num ? lastCreated.detail : null;
    lastCreated.detail = null;
    void load();
  });

  const doc = $derived(detail?.doc);
  const num = $derived(doc?.num ?? 0);

  // Live: reload when this document or one it shows changed.
  let seenSeq = store.docSeq;
  $effect(() => {
    const seq = store.docSeq;
    if (seq === seenSeq) return;
    seenSeq = seq;
    const d = detail;
    if (!d) return;
    const refs = [d.parent, d.superseded_by, d.supplements, ...(d.children ?? []), ...(d.replaces ?? []), ...(d.supplemented_by ?? [])].filter(Boolean) as DocRef[];
    if (concerns(store.docChanged, [d.doc.num, ...refs.map((r) => r.num)])) void load();
  });

  // The kinds (who activates, required sections; the extract form's kinds).
  let kinds = $state.raw<DocKind[]>([]);
  $effect(() => {
    get<{ kinds: DocKind[] }>('/v1/docs/kinds')
      .then((r) => (kinds = r.kinds))
      .catch(() => undefined);
  });
  const kind = $derived(kinds.find((k) => k.name === doc?.kind));

  /** Run a change; its answer is the new record. */
  async function act(f: () => Promise<DocDetail>, done?: (d: DocDetail) => string | null) {
    busy = true;
    error = null;
    note = null;
    try {
      const d = await f();
      detail = d;
      note = done ? done(d) : null;
      return true;
    } catch (e) {
      error = docError(e);
      return false;
    } finally {
      busy = false;
    }
  }

  // --- status, pin, verify, supersede ---------------------------------------
  let verifyFor = $state('');
  let supersedeBy = $state('');
  const setStatus = (status: 'active' | 'archived' | 'draft') => act(() => patchDoc(num, { status }));
  const setPinned = (pinned: boolean) => act(() => patchDoc(num, { pinned }));
  const verify = () => act(() => docAction(num, 'verify', verifyFor ? { for: verifyFor } : {}), () => t('doc.verified'));
  async function supersede(e: SubmitEvent) {
    e.preventDefault();
    const by = supersedeBy.trim();
    if (!by) return;
    if (await act(() => docAction(num, 'supersede', { by }))) supersedeBy = '';
  }

  // --- conflicts (409 stale) -----------------------------------------------
  /** A write refused because the document changed since it was loaded:
   * what changed, and the write to send again on top of the new revision. */
  let conflict = $state.raw<{ stale: DocStaleBody; retry: (rev: number) => Promise<void> } | null>(null);

  async function write(f: (ifRev: number) => Promise<DocDetail>, ifRev: number, retry: (rev: number) => Promise<void>): Promise<boolean> {
    busy = true;
    error = null;
    note = null;
    try {
      const d = await f(ifRev);
      detail = d;
      conflict = null;
      note = d.unchanged ? t('doc.unchanged') : null;
      if (d.dangling?.length) note = `${note ? `${note} ` : ''}${t('doc.danglingAfter', { list: d.dangling.map((l) => l.target).join(', ') })}`;
      return true;
    } catch (e) {
      const s = staleBody(e);
      if (s) conflict = { stale: s, retry };
      else error = docError(e);
      return false;
    } finally {
      busy = false;
    }
  }

  // --- body ------------------------------------------------------------------
  let editing = $state(false);
  let bodyDraft = $state('');
  let bodyMessage = $state('');
  let bodyRev = $state(0);

  function startEdit() {
    if (!doc) return;
    bodyDraft = doc.body ?? '';
    bodyMessage = '';
    bodyRev = doc.rev;
    editing = true;
    conflict = null;
  }

  async function saveBody(rev: number) {
    const body = bodyDraft;
    const message = bodyMessage.trim() || undefined;
    const ok = await write((r) => writeBody(num, { body, message, if_rev: r }), rev, (r) => saveBody(r));
    if (ok) editing = false;
  }

  // --- fields ----------------------------------------------------------------
  let editingFields = $state(false);
  let fTitle = $state('');
  let fDesc = $state('');
  let fSlug = $state('');
  let fOwner = $state('');
  let fTeam = $state('');
  let fLabels = $state('');
  let fEditors = $state('');

  function startFields() {
    if (!doc) return;
    fTitle = doc.title;
    fDesc = doc.description ?? '';
    fSlug = doc.slug ?? '';
    fOwner = doc.owner;
    fTeam = doc.team ?? '';
    fLabels = (doc.labels ?? []).join(', ');
    fEditors = (doc.editors ?? []).join(', ');
    editingFields = true;
    conflict = null;
  }

  function fieldChanges(): Record<string, unknown> {
    if (!doc) return {};
    const b: Record<string, unknown> = {};
    if (fTitle.trim() !== doc.title) b.title = fTitle.trim();
    if (fDesc.trim() !== (doc.description ?? '')) b.description = fDesc.trim();
    if (fSlug.trim() !== (doc.slug ?? '')) b.slug = fSlug.trim() || null;
    if (fOwner !== doc.owner) b.owner = fOwner;
    if (fTeam !== (doc.team ?? '')) b.team = fTeam || null;
    const labels = parseList(fLabels);
    if (labels.join(',') !== (doc.labels ?? []).join(',')) b.labels = labels;
    const editors = parseList(fEditors);
    if (editors.join(',') !== (doc.editors ?? []).join(',')) b.editors = editors;
    return b;
  }

  async function saveFields(rev: number) {
    const b = fieldChanges();
    if (!Object.keys(b).length) {
      editingFields = false;
      return;
    }
    const ok = await write((r) => patchDoc(num, { ...b, if_rev: r }), rev, (r) => saveFields(r));
    if (ok) editingFields = false;
  }

  // --- split, extract, merge -------------------------------------------------
  let splitLevel = $state(1);
  let exAnchor = $state('');
  let exTitle = $state('');
  let exKind = $state('');
  let mergeInto = $state('');

  const moved = (d: DocDetail) => (d.moved?.length ? t('doc.moved', { list: d.moved.map((m) => `${m.from} → ${m.to}`).join(', ') }) : null);

  async function split() {
    if (!doc || !confirm(t('doc.split.confirm', { level: splitLevel }))) return;
    await act(() => docAction(num, 'split', { level: splitLevel, if_rev: doc.rev }), moved);
  }

  async function extract(e: SubmitEvent) {
    e.preventDefault();
    if (!doc || !exAnchor || !exTitle.trim()) return;
    const ok = await act(() => docAction(num, 'extract', { anchor: exAnchor, into: exTitle.trim(), kind: exKind || undefined, if_rev: doc.rev }), moved);
    if (ok) {
      exAnchor = '';
      exTitle = '';
    }
  }

  async function merge(e: SubmitEvent) {
    e.preventDefault();
    const into = mergeInto.trim();
    if (!doc || !into || !confirm(t('doc.merge.confirm', { into }))) return;
    if (await act(() => docAction(num, 'merge', { into, if_rev: doc.rev }), moved)) mergeInto = '';
  }

  // --- discussion ------------------------------------------------------------
  let comment = $state('');
  let commentAnchor = $state('');
  let discussion = $state.raw<ThreadResponse | null>(null);
  let discussionVersion = $state(0);
  const threadId = $derived(doc?.thread);
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
        if (threadId === id) discussion = r;
      })
      .catch(() => (discussion = null));
  });
  const commentInfo = $derived(new Map((detail?.comments ?? []).map((c) => [c.message_id, c])));

  async function sendComment(e: SubmitEvent) {
    e.preventDefault();
    const body = comment.trim();
    if (!body) return;
    busy = true;
    error = null;
    note = null;
    try {
      const r = await post<DocCommentResponse>(docPath(num, '/comment'), { body, anchor: commentAnchor || undefined });
      comment = '';
      note = t('doc.commented', { id: r.id });
      discussionVersion++;
      await load();
      void store.mailChanged();
    } catch (err) {
      error = docError(err);
    } finally {
      busy = false;
    }
  }

  // Linked threads: the subject of their first message.
  let threadSubjects = $state.raw<Map<number, Message | null>>(new Map());
  const threadKey = $derived((detail?.threads ?? []).map((x) => x.thread_id).join(','));
  $effect(() => {
    const ids = threadKey ? threadKey.split(',').map(Number) : [];
    const next = new Map<number, Message | null>();
    void Promise.all(
      ids.map((id) =>
        get<ThreadResponse>(`/v1/threads/${id}`)
          .then((r) => next.set(id, r.messages[0] ?? null))
          .catch(() => next.set(id, null)),
      ),
    ).then(() => {
      if (ids.join(',') === threadKey) threadSubjects = next;
    });
  });

  // --- links -----------------------------------------------------------------
  function linkHref(l: DocLink): string | undefined {
    switch (l.kind) {
      case 'doc':
        return l.num ? docHref(l.num, l.anchor) : undefined;
      case 'task':
        return l.num ? href(['tasks', `t${l.num}`]) : undefined;
      case 'thread':
        return l.num ? href(['thread', l.num]) : undefined;
      case 'seat':
        return href(['org', l.target.replace(/^@/, '')]);
      default:
        return undefined;
    }
  }

  const sections = $derived(detail ? headedSections(detail.sections) : []);
  const relations = $derived.by(() => {
    if (!detail) return [];
    const one = (r?: DocRef) => (r ? [r] : []);
    return [
      { key: 'parent', list: one(detail.parent) },
      { key: 'children', list: detail.children ?? [] },
      { key: 'supersededBy', list: one(detail.superseded_by) },
      { key: 'replaces', list: detail.replaces ?? [] },
      { key: 'supplements', list: one(detail.supplements) },
      { key: 'supplementedBy', list: detail.supplemented_by ?? [] },
      { key: 'related', list: detail.related ?? [] },
    ].filter((g) => g.list.length) as { key: 'parent' | 'children' | 'supersededBy' | 'replaces' | 'supplements' | 'supplementedBy' | 'related'; list: DocRef[] }[];
  });
  const humanActivates = $derived(kind?.activate_by === 'human' || store.teams.find((x) => x.name === doc?.team)?.doc_activate_by === 'human');
  const indexTooLong = $derived(doc?.kind === 'index' && (doc.body?.length ?? 0) > 1000);
  const seats = $derived(store.seats.map((s) => s.name));
</script>

<div class="page docpage">
  <p class="back"><a href={href(['docs'])}>← {t('docs.title')}</a></p>
  {#if loadError}
    <p class="error" role="alert">{t('doc.notFound', { id: target })} ({loadError})</p>
  {:else if !detail || !doc}
    <p class="faint">{t('app.loading')}</p>
  {:else}
    <div class="page-head">
      <h1><span class="mono faint">{docId(doc.num)}</span> <Vis text={doc.title} /></h1>
      <span class="chip neutral" data-testid="doc-kind">{doc.kind}</span>
      <span class="chip tone-{docStatusTone(doc.status)}" data-testid="doc-status">{t(`doc.status.${doc.status}`)}</span>
      {#if doc.stale}<span class="chip attn" title={t('doc.stale.long')} data-testid="doc-stale">{t('doc.stale')}</span>{/if}
      {#if doc.pinned}<span class="chip ack">{t('doc.pinned')}</span>{/if}
      <span class="spacer"></span>
      {#if !editing}<button class="small" onclick={startEdit} disabled={doc.status === 'active' && doc.kind === 'decision'} title={doc.status === 'active' && doc.kind === 'decision' ? t('doc.frozen') : ''} data-testid="doc-edit">{t('doc.editBody')}</button>{/if}
      {#if !editingFields}<button class="small" onclick={startFields}>{t('doc.editFields')}</button>{/if}
    </div>
    {#if doc.description}<p class="desc muted"><Vis text={doc.description} /></p>{/if}
    {#if doc.advice}<p class="advice" role="note" data-testid="doc-advice">{doc.advice}</p>{/if}
    {#if indexTooLong}<p class="advice" role="note">{t('doc.indexLong')}</p>{/if}
    {#if created?.candidates_line}
      <div class="notice card" role="status" data-testid="doc-candidates">
        <span>{created.candidates_line}</span>
        <ul>{#each created.candidates ?? [] as c (c.doc.num)}<li><DocChip doc={c.doc} /></li>{/each}</ul>
      </div>
    {/if}
    {#if created?.dangling?.length}<p class="advice">{t('doc.danglingAfter', { list: created.dangling.map((l) => l.target).join(', ') })}</p>{/if}
    {#if error}<p class="error" role="alert" data-testid="doc-error">{error}</p>{/if}
    {#if note}<p class="ok" role="status" data-testid="doc-note">{note}</p>{/if}

    {#if conflict}
      <div class="conflict card" role="alert" data-testid="doc-conflict">
        <strong>{t('doc.conflict', { rev: conflict.stale.rev })}</strong>
        <ul class="since">
          {#each conflict.stale.since as ev (ev.seq)}
            <li><span class="mono">{ev.actor ?? t('doc.system')}</span> · {ev.kind}{#if ev.rev} · {t('doc.revN', { rev: ev.rev })}{/if}{#if ev.note} · <Vis text={ev.note} />{/if} · <span class="faint">{shortTime(ev.at, ui.now, ui.lang)}</span></li>
          {/each}
        </ul>
        <DocDiff diff={conflict.stale.diff} />
        <div class="row wrap">
          <button class="primary small" disabled={busy} onclick={() => conflict?.retry(conflict.stale.rev)} data-testid="doc-retry">{t('doc.conflict.retry', { rev: conflict.stale.rev })}</button>
          <button class="small" disabled={busy} onclick={() => { conflict = null; editing = false; editingFields = false; void load(); }}>{t('doc.conflict.discard')}</button>
        </div>
        <p class="faint hint">{t('doc.conflict.hint')}</p>
      </div>
    {/if}

    <div class="cols">
      <div class="mainc">
        {#if editingFields}
          <form class="card edit" onsubmit={(e) => { e.preventDefault(); void saveFields(doc.rev); }} data-testid="doc-fields-form">
            <label>{t('doc.field.title')}<input maxlength="512" required bind:value={fTitle} /></label>
            <label>{t('doc.field.description')}<input maxlength="200" bind:value={fDesc} /></label>
            <div class="row wrap">
              <label>{t('doc.field.slug')}<input class="mono" maxlength="128" placeholder="runbook.deploy" bind:value={fSlug} /></label>
              <label>{t('doc.field.owner')}
                <select bind:value={fOwner}>
                  {#each seats as s}<option value={s}>{s}</option>{/each}
                  {#if !seats.includes(doc.owner)}<option value={doc.owner}>{doc.owner}</option>{/if}
                </select>
              </label>
              <label>{t('doc.field.team')}
                <select bind:value={fTeam}>
                  <option value="">{t('doc.noTeam')}</option>
                  {#each store.teams as tm}<option value={tm.name}>{tm.name}</option>{/each}
                </select>
              </label>
            </div>
            <label>{t('doc.field.labels')}<input placeholder={t('task.labelsHint')} bind:value={fLabels} /></label>
            <label>{t('doc.field.editors')}<input class="mono" placeholder="w1, w2" bind:value={fEditors} /></label>
            <div class="row">
              <button class="primary small" type="submit" disabled={busy}>{t('admin.save')}</button>
              <button class="small" type="button" onclick={() => (editingFields = false)}>{t('admin.cancel')}</button>
            </div>
          </form>
        {/if}

        {#if editing}
          <form class="card edit" onsubmit={(e) => { e.preventDefault(); void saveBody(bodyRev); }} data-testid="doc-body-form">
            <label>{t('doc.body')}<textarea class="mono" rows="22" bind:value={bodyDraft} data-testid="doc-body-input"></textarea></label>
            <label>{t('doc.revMessage')}<input maxlength="500" placeholder={t('doc.revMessageHint')} bind:value={bodyMessage} /></label>
            <div class="row wrap">
              <button class="primary small" type="submit" disabled={busy}>{t('doc.saveRev', { rev: bodyRev })}</button>
              <button class="small" type="button" onclick={() => { editing = false; conflict = null; }}>{t('admin.cancel')}</button>
              <span class="faint hint">{t('doc.editHint')}</span>
            </div>
          </form>
        {:else if doc.body}
          <article class="card bodycard">
            <DocBody body={doc.body} sections={detail.sections} num={doc.num} {anchor} />
          </article>
        {:else}
          <p class="faint">{t('doc.noBody')}</p>
        {/if}

        {#each relations as g (g.key)}
          <section class="refs">
            <h2>{t(`doc.rel.${g.key}`)} <span class="count">{g.list.length}</span></h2>
            <ul>{#each g.list as r (r.num)}<li><DocChip doc={r} /></li>{/each}</ul>
          </section>
        {/each}

        <section class="refs" data-testid="doc-links">
          <h2>{t('doc.linksOut')} <span class="count">{detail.links.length}</span></h2>
          {#if detail.links.length}
            <table class="links">
              <tbody>
                {#each detail.links as l, i (i)}
                  {@const h = linkHref(l)}
                  <tr class:bad={l.dangling || l.stale}>
                    <td class="mono">{#if h}<a href={h}><Vis text={l.target} /></a>{:else}<Vis text={l.target} />{/if}</td>
                    <td>{#if l.title}<Vis text={l.title} />{/if}
                      {#if l.renamed}<span class="chip neutral" title={t('doc.renamed.long', { was: l.title_at_write ?? '' })}>{t('doc.renamed')}</span>{/if}
                      {#if l.dangling}<span class="chip tone-danger" title={t('doc.dangling.long')}>{t('doc.dangling')}</span>{/if}
                      {#if l.stale}<span class="chip attn" title={t('doc.linkStale.long')}>{t('doc.linkStale')}</span>{/if}
                    </td>
                    <td class="faint">{#if l.src_anchor}{t('doc.inSection', { anchor: l.src_anchor })}{/if}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {:else}<p class="faint">{t('doc.none')}</p>{/if}
          <h2>{t('doc.linksIn')} <span class="count">{detail.linked_from.length}</span></h2>
          {#if detail.linked_from.length}
            <ul>
              {#each detail.linked_from as l, i (i)}
                <li>
                  <a class="mono" href={docHref(l.src, l.src_anchor || undefined)}>{docId(l.src)}{#if l.src_anchor}#{l.src_anchor}{/if}</a>
                  <span class="faint">→ <Vis text={l.target} /></span>
                  {#if l.dangling}<span class="chip tone-danger" title={t('doc.dangling.long')}>{t('doc.dangling')}</span>{/if}
                </li>
              {/each}
            </ul>
          {:else}<p class="faint">{t('doc.none')}</p>{/if}
        </section>

        {#if detail.tasks?.length || detail.threads?.length}
          <section class="refs">
            {#if detail.tasks?.length}
              <h2>{t('doc.tasks')} <span class="count">{detail.tasks.length}</span></h2>
              <ul>{#each detail.tasks as x (x.num)}<TaskRefRow num={x.num} title={x.title} state={x.state} note={x.assignee} />{/each}</ul>
            {/if}
            {#if detail.threads?.length}
              <h2>{t('doc.threads')} <span class="count">{detail.threads.length}</span></h2>
              <ul>
                {#each detail.threads as th (th.thread_id)}
                  {@const m = threadSubjects.get(th.thread_id)}
                  <li><a class="mono" href={href(['thread', th.thread_id])}>#{th.thread_id}</a>
                    {#if m}<a href={href(['thread', th.thread_id])}>{#if m.subject}<Vis text={shortSubject(m.subject)} />{:else}{t('mail.noSubject')}{/if}</a>{/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </section>
        {/if}

        <section class="discussion" data-testid="doc-discussion">
          <h2 class="row">{t('doc.discussion')}{#if doc.thread}<a class="sublink" href={href(['thread', doc.thread])}>{t('task.openThread')}</a>{/if}</h2>
          {#if !doc.thread}
            <p class="faint">{t('doc.noDiscussion')}</p>
          {:else if discussion}
            <div class="msgs">
              {#each discussion.messages as m (m.id)}
                {@const c = commentInfo.get(m.id)}
                {#if c?.anchor}
                  <p class="on" data-testid="comment-anchor">
                    {#if c.removed}<span class="chip attn">{t('doc.onRemoved', { path: c.path || c.anchor })}</span>
                    {:else}{t('doc.onSection')} <a href={docHref(doc.num, c.anchor)}>#{c.anchor}</a>{#if c.path} <span class="faint">({c.path})</span>{/if}{/if}
                  </p>
                {/if}
                <MessageCard message={m}>
                  {#snippet actions()}
                    <button class="small" onclick={() => go(['compose'], { reply: m.id })}>{t('mail.reply')}</button>
                  {/snippet}
                </MessageCard>
              {/each}
            </div>
          {/if}
          <form class="comment" onsubmit={sendComment}>
            <label class="inline">{t('doc.commentOn')}
              <select bind:value={commentAnchor} aria-label={t('doc.commentOn')} data-testid="comment-anchor-select">
                <option value="">{t('doc.wholeDoc')}</option>
                {#each sections as s (s.anchor)}<option value={s.anchor}>{s.path}</option>{/each}
              </select>
            </label>
            <textarea rows="3" placeholder={t('doc.commentPlaceholder')} aria-label={t('doc.comment')} bind:value={comment}></textarea>
            <div class="row">
              <button class="primary small" type="submit" disabled={busy || !comment.trim()}>{t('doc.comment')}</button>
              <span class="faint hint">{t('doc.commentHint')}</span>
            </div>
          </form>
        </section>

        <DocHistory num={doc.num} version={detail.event_count} rev={doc.rev} />
      </div>

      <aside class="side card">
        <dl>
          <dt>{t('doc.field.status')}</dt>
          <dd>
            <div class="row wrap">
              {#if doc.status === 'draft'}
                <button class="small primary" disabled={busy} onclick={() => setStatus('active')} data-testid="doc-activate">{t('doc.activate')}</button>
              {/if}
              {#if doc.status === 'draft' || doc.status === 'active'}
                <button class="small" disabled={busy} onclick={() => setStatus('archived')} data-testid="doc-archive">{t('doc.archive')}</button>
              {/if}
            </div>
            {#if doc.status === 'draft' && humanActivates}<span class="faint hint">{t('doc.humanActivates')}</span>{/if}
            {#if doc.status === 'draft' || doc.status === 'active'}
              <form class="row wrap" onsubmit={supersede}>
                <DocPicker bind:value={supersedeBy} exclude={doc.num} label={t('doc.supersedeBy')} />
                <button class="small" type="submit" disabled={busy || !supersedeBy.trim()}>{t('doc.supersede')}</button>
              </form>
            {/if}
          </dd>
          <dt>{t('doc.verification')}</dt>
          <dd data-testid="doc-verification">
            {#if doc.verified_at}
              <span title={fullTime(doc.verified_at, ui.lang)}>{t('doc.verifiedBy', { seat: doc.verified_by ?? '—', time: shortTime(doc.verified_at, ui.now, ui.lang) })}</span>
            {:else}<span class="faint">{t('doc.neverVerified')}</span>{/if}
            {#if doc.expires_at}<span class:late={doc.stale} title={fullTime(doc.expires_at, ui.lang)}>{t('doc.expires', { time: shortTime(doc.expires_at, ui.now, ui.lang) })}</span>{/if}
            <div class="row wrap">
              <select bind:value={verifyFor} aria-label={t('doc.verifyFor')}>
                {#each VERIFY_INTERVALS as v}<option value={v}>{v ? (v === 'none' ? t('doc.verify.none') : v) : t('doc.verify.default', { days: kind?.verify_days ?? '—' })}</option>{/each}
              </select>
              <button class="small" disabled={busy} onclick={verify} data-testid="doc-verify">{t('doc.verify')}</button>
            </div>
          </dd>
          <dt>{t('doc.field.pinned')}</dt>
          <dd>
            <button class="small" disabled={busy} onclick={() => setPinned(!doc.pinned)} data-testid="doc-pin">{doc.pinned ? t('doc.unpin') : t('doc.pin')}</button>
          </dd>
          <dt>{t('doc.field.owner')}</dt>
          <dd><a class="mono" href={href(['org', doc.owner])}>{doc.owner}</a></dd>
          <dt>{t('doc.field.team')}</dt>
          <dd>{#if doc.team}<a class="mono" href={href(['docs'], { team: doc.team })}>{doc.team}</a>{:else}<span class="faint">{t('doc.noTeam')}</span>{/if}</dd>
          {#if doc.slug}<dt>{t('doc.field.slug')}</dt><dd class="mono">{doc.slug}</dd>{/if}
          <dt>{t('doc.field.labels')}</dt>
          <dd class="row wrap">{#each doc.labels ?? [] as l}<a class="chip neutral" href={href(['docs'], { label: l, status: 'all' })}>{l}</a>{:else}<span class="faint">—</span>{/each}</dd>
          <dt>{t('doc.field.editors')}</dt>
          <dd class="row wrap">{#each doc.editors ?? [] as e}<a class="mono" href={href(['org', e])}>{e}</a>{:else}<span class="faint">—</span>{/each}</dd>
          <dt>{t('doc.size')}</dt>
          <dd><span>{t('doc.tokens', { n: doc.tokens.toLocaleString() })} · {t('doc.bytes', { n: doc.bytes.toLocaleString() })}</span></dd>
          <dt>{t('doc.rev')}</dt>
          <dd class="mono" data-testid="doc-rev">{doc.rev}</dd>
          <dt>{t('task.created')}</dt>
          <dd title={fullTime(doc.created_at, ui.lang)}>{shortTime(doc.created_at, ui.now, ui.lang)}</dd>
          <dt>{t('task.updated')}</dt>
          <dd title={fullTime(doc.updated_at, ui.lang)}>{shortTime(doc.updated_at, ui.now, ui.lang)}</dd>
          {#if sections.length}
            <dt>{t('doc.sections')}</dt>
            <dd>
              <ol class="toc">
                {#each sections as s (s.anchor)}
                  <li style:padding-left={`${(s.level - 1) * 10}px`}><a href={docHref(doc.num, s.anchor)}><Vis text={s.heading} /></a> <span class="faint">~{s.tokens}</span></li>
                {/each}
              </ol>
            </dd>
          {/if}
          <dt>{t('doc.restructure')}</dt>
          <dd>
            <div class="row wrap">
              <select bind:value={splitLevel} aria-label={t('doc.split.level')}>
                <option value={1}>{t('doc.split.levelN', { level: 1 })}</option>
                <option value={2}>{t('doc.split.levelN', { level: 2 })}</option>
              </select>
              <button class="small" disabled={busy} onclick={split}>{t('doc.split')}</button>
            </div>
            <form class="stack" onsubmit={extract}>
              <select bind:value={exAnchor} aria-label={t('doc.extract.section')}>
                <option value="">{t('doc.extract.section')}</option>
                {#each sections as s (s.anchor)}<option value={s.anchor}>{s.path}</option>{/each}
              </select>
              <input maxlength="512" placeholder={t('doc.extract.title')} aria-label={t('doc.extract.title')} bind:value={exTitle} />
              <div class="row wrap">
                <select bind:value={exKind} aria-label={t('doc.field.kind')}>
                  <option value="">{t('doc.extract.sameKind')}</option>
                  {#each kinds.filter((k) => !k.disabled) as k (k.name)}<option value={k.name}>{k.name}</option>{/each}
                </select>
                <button class="small" type="submit" disabled={busy || !exAnchor || !exTitle.trim()}>{t('doc.extract')}</button>
              </div>
            </form>
            <form class="row wrap" onsubmit={merge}>
              <DocPicker bind:value={mergeInto} exclude={doc.num} label={t('doc.mergeInto')} />
              <button class="small" type="submit" disabled={busy || !mergeInto.trim()}>{t('doc.merge')}</button>
            </form>
            <span class="faint hint">{t('doc.restructureHint')}</span>
          </dd>
        </dl>
      </aside>
    </div>
  {/if}
</div>

<style>
  .docpage {
    max-width: 1280px;
  }
  .back {
    margin: 0 0 6px;
    font-size: 12px;
  }
  h1 {
    overflow-wrap: anywhere;
  }
  .desc {
    margin: 0 0 8px;
  }
  .advice {
    margin: 0 0 8px;
    padding: 5px 10px;
    border-radius: var(--radius);
    background: var(--warn-weak);
    color: var(--warn);
  }
  .notice,
  .conflict {
    padding: 8px 12px;
    margin-bottom: 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .notice {
    background: var(--notice-weak);
  }
  .notice ul,
  .since {
    margin: 0;
    padding-left: 18px;
  }
  .conflict {
    border-color: var(--danger);
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 320px;
    gap: 16px;
    align-items: start;
  }
  .mainc {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-width: 0;
  }
  .bodycard {
    padding: 14px 18px;
  }
  .edit {
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .edit label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  .edit textarea {
    width: 100%;
    font-size: 12px;
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
  .links {
    width: 100%;
    font-size: 12px;
    margin: 6px 0 10px;
  }
  .links tr.bad td:first-child {
    color: var(--danger);
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
  .on {
    margin: 4px 0 -4px;
    font-size: 12px;
  }
  .comment {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 6px;
  }
  .inline {
    display: flex;
    gap: 6px;
    align-items: center;
    font-size: 12px;
    color: var(--muted);
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
    min-width: 0;
  }
  dd.row {
    flex-direction: row;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: 100%;
  }
  .toc {
    list-style: none;
    margin: 0;
    padding: 0;
    font-size: 12px;
  }
  .late {
    color: var(--danger);
    font-weight: 600;
  }
  @media (max-width: 960px) {
    .cols {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  @media (max-width: 720px) {
    .docpage {
      padding: 12px 16px;
    }
    .bodycard {
      padding: 10px 12px;
    }
  }
</style>
