<script lang="ts">
  // Under a seat's conversation mirror: a prompt box (`POST
  // /seats/{name}/prompt`; the node gives the text to the session) and the
  // way to stop the current turn: an Interrupt button for Codex and dsh
  // seats, the terminal for Claude Code seats, which have no structured
  // interrupt.
  import { t } from '../lib/ui.svelte';
  import { interruptKind } from '../lib/requests';
  import { interruptSeat, promptSeat, requestError } from '../lib/requestops';
  import { terminalHref } from '../lib/presence';
  import type { Presence, Seat } from '../lib/types';

  let { seat, presence }: { seat: Seat; presence: Presence | undefined } = $props();

  let text = $state('');
  let busy = $state(false);
  let msg = $state<{ text: string; error: boolean } | null>(null);
  const how = $derived(interruptKind(seat.harness, presence?.harness));
  const terminal = $derived(terminalHref(presence?.terminal_url));

  async function send(e?: SubmitEvent) {
    e?.preventDefault();
    if (busy || !text.trim()) return;
    busy = true;
    msg = null;
    try {
      const r = await promptSeat(seat.name, text);
      text = '';
      msg = { text: t('control.queued', { n: r.nodes }), error: false };
    } catch (err) {
      msg = { text: requestError(err), error: true };
    } finally {
      busy = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void send();
    }
  }

  async function interrupt() {
    if (busy) return;
    busy = true;
    msg = null;
    try {
      const r = await interruptSeat(seat.name);
      msg = { text: t('control.interrupted', { n: r.nodes }), error: false };
    } catch (err) {
      msg = { text: requestError(err), error: true };
    } finally {
      busy = false;
    }
  }
</script>

<div class="controls" data-testid="seat-controls">
  <form class="prompt" onsubmit={send}>
    <textarea rows="2" maxlength="65536" placeholder={t('control.placeholder', { seat: seat.name })} aria-label={t('control.prompt')} bind:value={text} onkeydown={onKey}></textarea>
    <div class="row wrap btns">
      <button class="small primary" type="submit" disabled={busy || !text.trim()}>{t('control.send')}</button>
      {#if how === 'interrupt'}
        <button class="small danger" type="button" disabled={busy} onclick={interrupt} data-testid="interrupt">{t('control.interrupt')}</button>
      {:else}
        <span class="faint expl" data-testid="no-interrupt">
          {t('control.claudeNoInterrupt')}
          {#if terminal}<a href={terminal} target="_blank" rel="noopener noreferrer">{t('seat.openTerminal')} ↗</a>{:else}<code>amail attach {seat.name}</code>{/if}
        </span>
      {/if}
    </div>
  </form>
  {#if msg}<p class={msg.error ? 'error' : 'ok'} role="status" data-testid="control-msg">{msg.text}</p>{/if}
  <p class="faint hint">{t('control.hint')}</p>
</div>

<style>
  .controls {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-top: 6px;
    border-top: 1px solid var(--border);
    flex: none;
  }
  .prompt {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .prompt textarea {
    width: 100%;
  }
  .btns {
    gap: 6px;
    align-items: center;
  }
  .expl {
    font-size: 12px;
    display: inline-flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: baseline;
  }
  .controls p {
    margin: 0;
    font-size: 12px;
  }
</style>
