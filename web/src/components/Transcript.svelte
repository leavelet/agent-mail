<script lang="ts">
  import Vis from './Vis.svelte';
  import { visible } from '../lib/visible';
  import { onDestroy, tick } from 'svelte';
  import type { TranscriptEvent, TranscriptStatus } from '../lib/types';
  import { t, ui } from '../lib/ui.svelte';
  import { shortTime } from '../lib/time';
  import { enc } from '../lib/api';
  import { apiUrl } from '../lib/base';
  import { href } from '../lib/router.svelte';
  import { interleave, requestId, type MirrorMark } from '../lib/requests';

  /**
   * The read-only conversation mirror of a seat. With `seat`, it subscribes
   * to the server's transcript stream; tests pass `events`/`status` directly.
   */
  let {
    seat,
    events: initialEvents = [],
    status: initialStatus = null,
    marks = [],
  }: { seat?: string; events?: TranscriptEvent[]; status?: TranscriptStatus | null; marks?: MirrorMark[] } = $props();

  const MAX_EVENTS = 2000;
  // svelte-ignore state_referenced_locally
  let events = $state.raw<TranscriptEvent[]>(initialEvents);
  // svelte-ignore state_referenced_locally
  let status = $state.raw<TranscriptStatus | null>(initialStatus);
  let connected = $state(true);
  let follow = $state(true);
  let box: HTMLDivElement | undefined = $state();
  let es: EventSource | null = null;

  function add(batch: TranscriptEvent[]) {
    const next = events.concat(batch);
    events = next.length > MAX_EVENTS ? next.slice(next.length - MAX_EVENTS) : next;
  }

  $effect(() => {
    if (!seat || typeof EventSource === 'undefined') return;
    const src = new EventSource(apiUrl(`/v1/seats/${enc(seat)}/transcript`));
    es = src;
    // Every (re)connection replays the server's buffer: start from empty.
    src.onopen = () => {
      connected = true;
      events = [];
    };
    src.onerror = () => (connected = false);
    src.addEventListener('status', (e) => {
      try {
        status = JSON.parse((e as MessageEvent).data) as TranscriptStatus;
      } catch {
        // ignore
      }
    });
    src.addEventListener('reset', () => {
      events = [];
    });
    src.addEventListener('events', (e) => {
      try {
        const d = JSON.parse((e as MessageEvent).data) as { events: TranscriptEvent[] };
        add(d.events ?? []);
        connected = true;
      } catch {
        // ignore
      }
    });
    return () => {
      src.close();
      if (es === src) es = null;
    };
  });

  onDestroy(() => es?.close());

  // Keep following the end unless the reader scrolled up.
  $effect(() => {
    void events;
    if (!follow || !box) return;
    void tick().then(() => {
      if (box) box.scrollTop = box.scrollHeight;
    });
  });

  function onScroll() {
    if (!box) return;
    follow = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
  }

  function jump() {
    follow = true;
    if (box) box.scrollTop = box.scrollHeight;
  }

  const explain = $derived.by(() => {
    const s = status?.state;
    if (!s || s === 'streaming') return null;
    return t(`tr.${s}`);
  });

  /** How an event is labelled: mail the mail system put into the session,
   * an error that ended a turn, or another note of the harness. */
  function noticeKind(ev: TranscriptEvent): 'mail' | 'error' | 'system' {
    // Older nodes did not flag a failed dsh turn as an error.
    if (ev.error || (ev.role === 'system' && ev.text.startsWith('turn ended:'))) return 'error';
    return ev.text.trimStart().startsWith('[amail') ? 'mail' : 'system';
  }

  function firstLine(s: string): string {
    const line = s.split('\n', 1)[0];
    return line.length > 160 ? `${line.slice(0, 160)}…` : line;
  }
</script>

<div class="transcript">
  <div class="bar">
    <span class="title">{t('tr.title')}</span>
    <span class="chip neutral">{t('tr.readonly')}</span>
    {#if status?.state === 'streaming'}
      <span class="chip ok" class:off={!connected}><span class="live" aria-hidden="true"></span>{connected ? t('tr.streaming') : t('tr.disconnected')}</span>
    {/if}
    {#if status?.harness}<span class="faint">{status.harness}</span>{/if}
    {#if status?.path}<span class="faint mono path" title={status.path}>{status.path.split('/').pop()}</span>{/if}
    {#if status?.skipped}<span class="faint">{t('tr.skipped', { n: status.skipped })}</span>{/if}
    <span class="spacer"></span>
    {#if !follow}<button class="small" onclick={jump}>↓ {t('tr.jump')}</button>{/if}
  </div>
  <div class="log" bind:this={box} onscroll={onScroll} aria-live="off" data-testid="transcript-log">
    {#if explain && events.length === 0}
      <div class="explain" data-state={status?.state}>
        {#if status?.state === 'waiting'}<span class="spinner" aria-hidden="true"></span>{/if}
        <p>{explain}</p>
        {#if status?.message}<p class="faint">{status.message}</p>{/if}
      </div>
    {:else if !status && events.length === 0}
      <div class="explain" data-state="connecting"><span class="spinner" aria-hidden="true"></span><p>{t('tr.waiting')}</p></div>
    {:else if events.length === 0}
      <div class="explain"><p>{t('tr.empty')}</p></div>
    {:else}
      {#if explain}<div class="inline-status">{explain}</div>{/if}
      {#each interleave(events, marks) as item}
        {#if 'mark' in item}
          {@const m = item.mark}
          <div class="mark mk-{m.what}" data-testid="tr-mark" data-request={m.num} data-what={m.what}>
            <span class="ts faint">{shortTime(new Date(m.at).toISOString(), ui.now, ui.lang)}</span>
            <span>
              <a class="mono" href={href(['requests', requestId(m.num)])}>{requestId(m.num)}</a>
              {t(`tr.mark.${m.what}`, { by: m.by ?? '' })}<Vis text={m.what === 'created' ? m.title : (m.answer ?? '')} />
            </span>
          </div>
        {:else}
        {@const ev = item.ev}
        {@const nk = ev.kind === 'notice' ? noticeKind(ev) : null}
        <div class="ev {ev.role} k-{ev.kind}" class:err={ev.error} class:failed={nk === 'error'} data-notice={nk}>
          <div class="gutter">
            <span class="who">
              {#if nk === 'mail'}{t('tr.notice')}{:else if nk === 'error'}{t('tr.error')}{:else if nk === 'system'}{t('tr.role.system')}{:else}{t(`tr.role.${ev.role}`)}{/if}
            </span>
            {#if ev.ts}<span class="ts faint">{shortTime(ev.ts, ui.now, ui.lang)}</span>{/if}
          </div>
          <div class="body">
            {#if ev.kind === 'tool_call'}
              <details>
                <summary><span class="tool mono">{visible(ev.tool ?? t('tr.call'))}</span> <span class="mono preview"><Vis text={firstLine(ev.text)} /></span></summary>
                <pre><Vis text={ev.text} /></pre>
              </details>
            {:else if ev.kind === 'tool_result'}
              <details>
                <summary>
                  <span class="faint">{t('tr.result')}{ev.tool ? ` · ${visible(ev.tool)}` : ''}</span>
                  {#if ev.error}<span class="chip attn">{t('tr.error')}</span>{/if}
                  <span class="mono preview"><Vis text={firstLine(ev.text)} /></span>
                </summary>
                <pre><Vis text={ev.text} /></pre>
              </details>
            {:else}
              <pre><Vis text={ev.text} /></pre>
            {/if}
            {#if ev.truncated}<span class="chip neutral trunc">✂ {t('tr.truncated')}</span>{/if}
          </div>
        </div>
        {/if}
      {/each}
    {/if}
  </div>
  <div class="note faint">{t('tr.note')}</div>
</div>

<style>
  .transcript {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
    min-width: 0;
  }
  .title {
    font-weight: 600;
  }
  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 260px;
  }
  .live {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ok);
    animation: blink 1.4s ease-in-out infinite;
  }
  .off .live {
    background: var(--warn);
    animation: none;
  }
  @keyframes blink {
    50% {
      opacity: 0.3;
    }
  }
  .mark {
    display: flex;
    gap: 8px;
    align-items: baseline;
    margin: 4px 10px;
    padding: 3px 8px;
    border-left: 3px solid var(--accent);
    background: var(--accent-weak);
    border-radius: 4px;
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .mark.mk-answered,
  .mark.mk-expired {
    border-left-color: var(--ok);
  }
  .mark.mk-withdrawn {
    border-left-color: var(--faint);
    background: var(--surface-2);
  }
  .log {
    flex: 1;
    overflow-y: auto;
    min-height: 120px;
    padding: 6px 0;
  }
  .explain {
    padding: 28px 20px;
    text-align: center;
    color: var(--muted);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
  }
  .explain p {
    margin: 0;
    max-width: 460px;
  }
  .inline-status {
    margin: 0 10px 6px;
    padding: 4px 8px;
    border-radius: 4px;
    background: var(--warn-weak);
    color: var(--warn);
  }
  .spinner {
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: 2px solid var(--border-strong);
    border-top-color: var(--accent);
    animation: spin 0.9s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .ev {
    display: grid;
    grid-template-columns: 74px 1fr;
    gap: 8px;
    padding: 3px 10px;
  }
  .gutter {
    display: flex;
    flex-direction: column;
    font-size: 11px;
    line-height: 16px;
    padding-top: 1px;
  }
  .who {
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--muted);
  }
  .ts {
    font-variant-numeric: tabular-nums;
  }
  .body {
    min-width: 0;
  }
  .body pre {
    font-family: var(--font);
    font-size: 13px;
  }
  .user .who {
    color: var(--src-user);
  }
  .user.k-text .body {
    border-left: 3px solid var(--src-user);
    padding-left: 8px;
  }
  .assistant .who {
    color: var(--ok);
  }
  .k-notice .who {
    color: var(--notice);
  }
  .k-notice .body {
    background: var(--notice-weak);
    border: 1px dashed var(--notice);
    border-radius: 5px;
    padding: 4px 8px;
  }
  .failed .who {
    color: var(--danger);
    font-weight: 700;
  }
  .k-notice.failed .body {
    background: var(--danger-weak);
    border: 1px solid var(--danger);
    color: var(--danger);
  }
  .k-notice .body pre {
    font-family: var(--mono);
    font-size: 12px;
  }
  .k-tool_call .body,
  .k-tool_result .body {
    font-size: 12px;
  }
  .tool {
    font-weight: 600;
    color: var(--accent);
  }
  details > summary {
    cursor: pointer;
    list-style: none;
    display: flex;
    gap: 6px;
    align-items: baseline;
    min-width: 0;
  }
  details > summary::before {
    content: '▸';
    color: var(--faint);
  }
  details[open] > summary::before {
    content: '▾';
  }
  .preview {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    min-width: 0;
  }
  details pre {
    margin: 4px 0 2px;
    padding: 6px 8px;
    background: var(--surface-2);
    border-radius: 4px;
    font-family: var(--mono) !important;
    font-size: 12px !important;
    max-height: 420px;
    overflow: auto;
  }
  .err .preview {
    color: var(--danger);
  }
  .tool + .preview,
  .k-tool_result .who,
  .k-tool_call .who {
    color: var(--faint);
  }
  .trunc {
    margin-top: 2px;
  }
  .note {
    font-size: 11px;
    padding: 4px 10px;
    border-top: 1px solid var(--border);
  }
</style>
