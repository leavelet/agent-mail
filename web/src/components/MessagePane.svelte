<script lang="ts">
  import { get, enc } from '../lib/api';
  import type { Delivery, Message, MessageResponse } from '../lib/types';
  import { t } from '../lib/ui.svelte';
  import { href, go } from '../lib/router.svelte';
  import { ack, markRead } from '../lib/mailops';
  import MessageCard from './MessageCard.svelte';

  /** One message (peeked: opening does not mark it read) with mail actions. */
  let { id, onchange }: { id: number; onchange?: () => void } = $props();

  let message = $state.raw<Message | null>(null);
  let delivery = $state.raw<Delivery | undefined>(undefined);
  let error = $state<string | null>(null);
  let busy = $state(false);

  $effect(() => {
    const mid = id;
    message = null;
    error = null;
    get<MessageResponse>(`/v1/messages/${enc(String(mid))}?peek=1`)
      .then((r) => {
        if (mid !== id) return;
        message = r.message;
        delivery = r.delivery;
      })
      .catch((e) => (error = e instanceof Error ? e.message : String(e)));
  });

  async function run(f: () => Promise<Delivery>) {
    busy = true;
    try {
      delivery = await f();
      onchange?.();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  export function doMarkRead() {
    if (message && delivery && !delivery.read_at) void run(() => markRead(id));
  }
  export function doAck() {
    if (message?.ack_required && delivery && !delivery.acked_at) void run(() => ack(id));
  }
  export function doReply(all = false) {
    go(['compose'], { reply: id, all: all ? 1 : undefined });
  }
</script>

{#if error}
  <p class="error pad">{error}</p>
{:else if !message}
  <p class="faint pad">{t('app.loading')}</p>
{:else}
  <MessageCard {message} showThreadLink>
    {#snippet actions()}
      <button class="small primary" onclick={() => doReply(false)}>{t('mail.reply')} <kbd>r</kbd></button>
      {#if message && (message.to.length + message.cc.length > 1)}
        <button class="small" onclick={() => doReply(true)}>{t('mail.replyAll')}</button>
      {/if}
      {#if message?.ack_required && delivery}
        <button class="small" disabled={busy || !!delivery.acked_at} onclick={doAck}>
          {delivery.acked_at ? t('mail.acked') : t('mail.ack')} <kbd>a</kbd>
        </button>
      {/if}
      {#if delivery}
        <button class="small" disabled={busy || !!delivery.read_at} onclick={doMarkRead}>{t('mail.markRead')} <kbd>e</kbd></button>
      {/if}
      <a class="small-link" href={href(['thread', message!.thread_id], { m: message!.id })}>{t('mail.thread')} →</a>
    {/snippet}
  </MessageCard>
{/if}

<style>
  .pad {
    padding: 16px;
  }
  .small-link {
    font-size: 12px;
    margin-left: 4px;
  }
  kbd {
    margin-left: 3px;
    font-size: 10px;
    padding: 0 3px;
    opacity: 0.75;
  }
</style>
