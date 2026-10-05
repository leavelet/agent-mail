<script lang="ts">
  import Vis from './Vis.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { shortTime } from '../lib/time';
  import { noteKey } from '../lib/receipt';
  import type { RecipientNote, SendResponse } from '../lib/types';

  /** What happened to a message just sent. */
  let { r, onagain }: { r: SendResponse; onagain?: () => void } = $props();

  function note(n: RecipientNote): string {
    const { key, params } = noteKey(n);
    if (params.until !== undefined) params.until = shortTime(params.until, ui.now, ui.lang);
    return t(key, params);
  }
</script>

<section class="card receipt" role="status" data-testid="send-receipt">
  <h2>✓ {t('compose.receipt.title', { id: r.id, n: r.deliveries })}</h2>
  <ul class="addrs">
    {#each r.recipients as a}
      <li data-address={a.address}>
        <span class="mono"><Vis text={a.address} /></span>{#if a.added} <span class="chip neutral" data-testid="added" title={a.note ?? t('compose.receipt.added.long')}>{t('compose.receipt.added')}</span>{#if a.note} <span class="muted" data-testid="added-note">{a.note}</span>{/if}{/if}:
        {#if a.wakes !== undefined}
          <span title={t('compose.receipt.listWakes.long')}>{t('compose.receipt.listWakes', { n: a.seats, w: a.wakes })}</span>
        {:else}
          {t('compose.receipt.seats', { n: a.seats })}
        {/if}
      </li>
    {/each}
    {#each r.rejected as x}
      <li class="error" data-rejected={x.address}>{t('compose.receipt.rejected', { address: x.address, reason: x.reason })}{#if x.reason === 'unreachable'} <span class="faint">{t('compose.unreachable')}</span>{:else if x.reason === 'thread_closed'} <span class="faint">{t('compose.threadClosed')}</span>{/if}</li>
    {/each}
  </ul>
  {#if r.notes?.length}
    <p class="lbl">{t('compose.receipt.notes')}</p>
    <ul class="notes">
      {#each r.notes as n}
        <li data-note={n.state}><a class="mono" href={href(['org', n.seat])}>{n.seat}</a> {note(n)}</li>
      {/each}
    </ul>
  {:else if r.deliveries > 0}
    <p class="faint">{t('compose.receipt.allSoon')}</p>
  {/if}
  <div class="row">
    <a class="btnlike primary" href={href(['thread', r.thread_id], { m: r.id })}>{t('compose.receipt.open')}</a>
    <button type="button" onclick={() => onagain?.()}>{t('compose.receipt.again')}</button>
  </div>
</section>

<style>
  .receipt {
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  h2 {
    font-size: 15px;
    margin: 0;
    color: var(--ok);
  }
  ul {
    margin: 0;
    padding-left: 18px;
  }
  .lbl {
    margin: 4px 0 0;
    font-weight: 600;
  }
  .notes li {
    color: var(--text);
  }
  p {
    margin: 0;
  }
  .btnlike {
    display: inline-block;
    border-radius: var(--radius);
    padding: 3px 10px;
    line-height: 20px;
    border: 1px solid var(--accent);
  }
  .btnlike.primary {
    background: var(--accent);
    color: var(--accent-text);
    font-weight: 600;
  }
  .btnlike:hover {
    text-decoration: none;
  }
</style>
