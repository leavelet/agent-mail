<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { Message } from '../lib/types';
  import { t, ui } from '../lib/ui.svelte';
  import { fullTime } from '../lib/time';
  import { href } from '../lib/router.svelte';
  import MetaView from './MetaView.svelte';
  import DeliveryTable from './DeliveryTable.svelte';

  let {
    message,
    actions,
    focus = false,
    showThreadLink = false,
  }: { message: Message; actions?: Snippet; focus?: boolean; showThreadLink?: boolean } = $props();

  let showDelivery = $state(false);
  const own = $derived(message.source !== 'external');
  const localSender = $derived(!message.from.includes('@'));
</script>

<article class="msg" class:focus data-msg={message.id} aria-labelledby="subj-{message.id}">
  <header>
    <div class="line1">
      <span class="src src-{message.source}" title={t(`mail.source.${message.source}.long`)}>{t(`mail.source.${message.source}`)}</span>
      {#if localSender}
        <a class="from mono" href={href(['org', message.from])}>{message.from}</a>
      {:else}
        <span class="from mono">{message.from}</span>
      {/if}
      {#if message.priority === 'urgent'}<span class="chip urgent">{t('mail.urgent')}</span>{/if}
      {#if message.ack_required}<span class="chip ack">{t('mail.ackReq')}</span>{/if}
      <span class="spacer"></span>
      <span class="faint id">#{message.id}</span>
      <time class="faint" datetime={message.created_at}>{fullTime(message.created_at, ui.lang)}</time>
    </div>
    <h2 id="subj-{message.id}">{message.subject || t('mail.noSubject')}</h2>
    <div class="rcpt">
      <span class="k">{t('mail.to')}</span><span class="mono">{message.to.join(', ') || '—'}</span>
      {#if message.cc.length}<span class="k">{t('mail.cc')}</span><span class="mono">{message.cc.join(', ')}</span>{/if}
      {#if message.in_reply_to}<span class="faint">{t('mail.inReplyTo', { id: message.in_reply_to })}</span>{/if}
      {#if showThreadLink}<a href={href(['thread', message.thread_id], { m: message.id })}>{t('mail.thread')} #{message.thread_id}</a>{/if}
    </div>
    {#if message.meta && Object.keys(message.meta).length}
      <MetaView meta={message.meta} />
    {/if}
  </header>
  {#if message.expired && !message.body}
    <p class="expired faint">{t('msg.expired')}</p>
  {:else}
    <pre class="body">{message.body}</pre>
  {/if}
  <footer class="row wrap">
    {@render actions?.()}
    {#if own}
      <button class="small" aria-expanded={showDelivery} onclick={() => (showDelivery = !showDelivery)}>
        {showDelivery ? t('mail.hideDelivery') : t('mail.delivery')}
      </button>
    {/if}
  </footer>
  {#if showDelivery}<DeliveryTable id={message.id} {message} />{/if}
</article>

<style>
  .msg {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 10px 14px 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .msg.focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  header {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .line1 {
    display: flex;
    align-items: center;
    gap: 7px;
    flex-wrap: wrap;
  }
  .from {
    font-weight: 600;
    font-size: 13px;
  }
  .id {
    font-variant-numeric: tabular-nums;
  }
  h2 {
    font-size: 15px;
  }
  .rcpt {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 8px;
    font-size: 12px;
  }
  .rcpt .k {
    color: var(--muted);
  }
  .src {
    font-size: 10.5px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 0 5px;
    border-radius: 3px;
    border: 1px solid currentColor;
    line-height: 16px;
  }
  .src-user {
    color: var(--src-user);
  }
  .src-agent {
    color: var(--src-agent);
  }
  .src-system {
    color: var(--src-system);
    background: color-mix(in srgb, var(--src-system) 12%, transparent);
  }
  .src-external {
    color: var(--src-external);
    background: color-mix(in srgb, var(--src-external) 12%, transparent);
  }
  .expired {
    margin: 6px 0;
    font-style: italic;
  }
  .body {
    font-family: var(--font);
    font-size: 13.5px;
    line-height: 1.55;
    padding: 2px 0;
  }
  footer {
    gap: 6px;
  }
</style>
