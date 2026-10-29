<script lang="ts">
  // One request for the person, with its answer form: the options as
  // buttons (the default marked), free text when the request takes it, an
  // optional note. Answers carry the revision the card shows (`if_rev`); a
  // request answered meanwhile shows who answered it and how.
  //
  // `mode`: `card` in "needs you" (body collapsed, Snooze), `page` on the
  // request page (body shown, no title link), `strip` above a message of
  // the request's thread (no body).
  import Vis from './Vis.svelte';
  import DocBody from './DocBody.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { store } from '../lib/store.svelte';
  import { duration, fullTime, shortTime } from '../lib/time';
  import { KNOWN_ATTENTION, parseTime, terminalHref } from '../lib/presence';
  import { answerText, optionOf, requestId } from '../lib/requests';
  import { answerRequest, conflictRequest, requestError } from '../lib/requestops';
  import { taskId } from '../lib/tasks';
  import type { Request, RequestOption } from '../lib/types';

  let {
    request,
    mode = 'card',
    onresolved,
    ondismiss,
  }: {
    request: Request;
    mode?: 'card' | 'page' | 'strip';
    /** The request is no longer open (answered here, or found answered). */
    onresolved?: (r: Request) => void;
    /** Shown on a resolved card: remove it from the list. */
    ondismiss?: () => void;
  } = $props();

  // The request as this card last saw it from its own calls; a newer
  // revision from the parent replaces it.
  let local = $state.raw<Request | null>(null);
  const r = $derived(local && local.num === request.num && local.rev >= request.rev ? local : request);
  let busy = $state(false);
  let error = $state<string | null>(null);
  /** Someone else answered first. */
  let lost = $state(false);
  let text = $state('');
  let note = $state('');
  let noteOpen = $state(false);
  let terminal = $state<string | null>(null);
  let noTerminal = $state(false);

  const open = $derived(r.state === 'open');
  const options = $derived(r.options ?? []);
  const attention = $derived(r.kind === 'attention');
  const age = $derived(duration(ui.now - parseTime(r.created_at), ui.lang));
  const def = $derived(optionOf(r, r.default));

  function label(o: RequestOption): string {
    // The system's options of an `attention` request have words here.
    if (attention && ['restart', 'sleep', 'open_terminal', 'dismiss'].includes(o.key)) return t(`req.attn.${o.key}` as 'req.attn.restart');
    return o.label;
  }

  function attnLabel(a: string): string {
    return KNOWN_ATTENTION.includes(a) ? t(`attn.${a}` as 'attn.login') : a;
  }

  async function answer(body: { option?: string; text?: string }) {
    if (busy) return;
    busy = true;
    error = null;
    // Opened at once, within the click, so the browser lets it through;
    // filled in when the server answers with the link.
    const win = body.option === 'open_terminal' && attention ? window.open('about:blank', '_blank') : null;
    try {
      const d = await answerRequest(r.num, { ...body, note: note.trim() || undefined, if_rev: r.rev });
      local = d.request;
      const url = terminalHref(d.terminal_url);
      if (body.option === 'open_terminal') {
        if (url) {
          terminal = url;
          if (win) {
            win.opener = null;
            win.location.href = url;
          }
        } else {
          win?.close();
          noTerminal = true;
        }
      }
      text = '';
      note = '';
      noteOpen = false;
      onresolved?.(d.request);
    } catch (e) {
      win?.close();
      const cur = conflictRequest(e);
      if (cur) {
        local = cur;
        if (cur.state === 'open') error = t('req.err.stale');
        else {
          lost = true;
          onresolved?.(cur);
        }
      } else error = requestError(e);
    } finally {
      busy = false;
    }
  }

  function sendText(e: SubmitEvent) {
    e.preventDefault();
    if (text.trim()) void answer({ text: text.trim() });
  }
</script>

<article class="rcard mode-{mode} cls-{r.class}" class:closed={!open} data-request={r.num} data-state={r.state} aria-labelledby="rq-{mode}-{r.num}">
  <header>
    <div class="l1">
      <span class="chip cls" title={t(`req.class.${r.class}.long`)}>{t(`req.class.${r.class}`)}</span>
      {#if r.kind !== 'question'}<span class="chip neutral kind">{t(`req.kind.${r.kind}`)}</span>{/if}
      <a class="mono rid" href={href(['requests', requestId(r.num)])}>{requestId(r.num)}</a>
      <a class="mono who" href={href(['org', r.seat])}>{r.seat}</a>
      {#if r.task}<a class="mono" href={href(['tasks', taskId(r.task)])} data-testid="req-task">{taskId(r.task)}</a>{/if}
      {#if attention && r.attention}<span class="chip attn">⚠ {attnLabel(r.attention)}</span>{/if}
      <span class="spacer"></span>
      <span class="faint age" title={fullTime(r.created_at, ui.lang)}>{t('req.age', { d: age })}</span>
    </div>
    {#if mode === 'page'}
      <h2 id="rq-{mode}-{r.num}" class="title"><Vis text={r.title} /></h2>
    {:else}
      <h3 id="rq-{mode}-{r.num}" class="title"><a href={href(['requests', requestId(r.num)])}><Vis text={r.title} /></a></h3>
    {/if}
    {#if open && r.deadline}
      <p class="deadline faint" data-testid="req-deadline" title={fullTime(r.deadline, ui.lang)}>
        {#if def}{t('req.defaultAt', { label: label(def), time: shortTime(r.deadline, ui.now, ui.lang) })}{:else}{t('req.remindAt', { time: shortTime(r.deadline, ui.now, ui.lang) })}{/if}
      </p>
    {/if}
  </header>

  {#if r.body && mode !== 'strip'}
    {#if mode === 'page'}
      <div class="body"><DocBody body={r.body} sections={[]} num={null} /></div>
    {:else}
      <details class="body">
        <summary>{t('req.details')}</summary>
        <DocBody body={r.body} sections={[]} num={null} />
      </details>
    {/if}
  {/if}

  {#if !open}
    <p class="outcome" data-testid="req-outcome" role="status">
      {#if lost}<strong>{t('req.lost')}</strong>{/if}
      {#if r.state === 'answered' && r.answer}
        {t('req.answeredBy', { by: r.answer.by, time: shortTime(r.answer.at, ui.now, ui.lang) })} <strong class="ans"><Vis text={answerText(r)} /></strong>
        {#if r.answer.note}<span class="faint">· <Vis text={r.answer.note} /></span>{/if}
      {:else if r.state === 'expired'}
        {t('req.expired')} <strong class="ans"><Vis text={answerText(r)} /></strong>
      {:else}
        {t(`req.state.${r.state}.long`)}
      {/if}
    </p>
    {#if terminal}<p><a href={terminal} target="_blank" rel="noopener noreferrer" data-testid="req-terminal">{t('seat.openTerminal')} ↗</a></p>{/if}
    {#if noTerminal}<p class="faint">{t('req.noTerminal', { cmd: `amail attach ${r.seat}` })}</p>{/if}
    {#if ondismiss}<button class="small ghost dismiss" onclick={ondismiss}>{t('req.hide')}</button>{/if}
  {:else}
    {#if options.length}
      <div class="opts" role="group" aria-label={t('req.options')}>
        {#each options as o, i (o.key)}
          <button
            class="opt small"
            class:primary={i === 0 && !def}
            class:def={o.key === r.default}
            data-option={o.key}
            title={o.description ?? ''}
            disabled={busy}
            onclick={() => answer({ option: o.key })}
          >
            <Vis text={label(o)} />{#if o.key === r.default}<span class="defmark">{t('req.default')}</span>{/if}
          </button>
        {/each}
      </div>
      {#if mode === 'page' && options.some((o) => o.description)}
        <dl class="descs">
          {#each options.filter((o) => o.description) as o (o.key)}<dt><Vis text={label(o)} /></dt><dd><Vis text={o.description ?? ''} /></dd>{/each}
        </dl>
      {/if}
    {/if}
    {#if r.free_text}
      <form class="free" onsubmit={sendText}>
        <textarea rows="2" maxlength="16384" placeholder={t('req.textPlaceholder')} aria-label={t('req.text')} bind:value={text}></textarea>
        <button class="small primary" type="submit" disabled={busy || !text.trim()}>{t('req.sendText')}</button>
      </form>
    {/if}
    <div class="row wrap tools">
      {#if noteOpen}
        <input class="note" maxlength="4000" placeholder={t('req.notePlaceholder')} aria-label={t('req.note')} bind:value={note} />
      {:else}
        <button class="small ghost" onclick={() => (noteOpen = true)}>{t('req.addNote')}</button>
      {/if}
      {#if mode === 'card'}
        <button class="small ghost" title={t('req.snooze.long')} onclick={() => store.snooze(r.num)}>{t('req.snooze')}</button>
      {/if}
    </div>
  {/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</article>

<style>
  .rcard {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    min-width: 0;
  }
  .rcard.cls-now {
    box-shadow: inset 3px 0 0 var(--danger);
  }
  .rcard.closed {
    opacity: 0.85;
    box-shadow: inset 3px 0 0 var(--ok);
  }
  .mode-page {
    padding: 14px 16px;
  }
  header {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .l1 {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px 8px;
    font-size: 12.5px;
  }
  .cls-now .cls {
    color: var(--danger);
    background: var(--danger-weak);
  }
  .cls-soon .cls {
    color: var(--warn);
    background: var(--warn-weak);
  }
  .cls-later .cls {
    color: var(--muted);
    background: var(--surface-2);
  }
  .who {
    font-weight: 650;
  }
  .age {
    font-size: 12px;
    white-space: nowrap;
  }
  .title {
    font-size: 14px;
    overflow-wrap: anywhere;
  }
  h2.title {
    font-size: 16px;
  }
  .deadline {
    margin: 0;
    font-size: 12px;
  }
  .body summary {
    cursor: pointer;
    color: var(--muted);
    font-size: 12px;
  }
  .body {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .opts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .opt {
    max-width: 100%;
    white-space: normal;
    text-align: left;
    min-height: 32px;
  }
  .opt.def {
    border-color: var(--accent);
  }
  .defmark {
    margin-left: 6px;
    font-size: 10.5px;
    font-weight: 700;
    text-transform: uppercase;
    color: var(--accent);
  }
  .free {
    display: flex;
    gap: 6px;
    align-items: flex-end;
    flex-wrap: wrap;
  }
  .free textarea {
    flex: 1;
    min-width: 0;
    width: 100%;
  }
  .tools {
    gap: 6px;
  }
  .note {
    flex: 1;
    min-width: 0;
  }
  .outcome {
    margin: 0;
  }
  .ans {
    overflow-wrap: anywhere;
  }
  .descs {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px 10px;
    margin: 0;
    font-size: 12.5px;
  }
  .descs dt {
    font-weight: 600;
  }
  .descs dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .dismiss {
    align-self: flex-start;
  }
  .error {
    margin: 0;
  }
</style>
