<script lang="ts">
  import { get, enc } from '../lib/api';
  import type { Delivery, Message, MessageResponse } from '../lib/types';
  import { t } from '../lib/ui.svelte';
  import { href, go } from '../lib/router.svelte';
  import { ack, markRead, markUnread } from '../lib/mailops';
  import MessageCard from './MessageCard.svelte';
  import RequestStrip from './RequestStrip.svelte';
  import { subjectRequestTags } from '../lib/requests';

  /** One message with mail actions. It is fetched without marking it read;
   * with `markOnOpen` it is then marked read (a person opened it). */
  let { id, onchange, markOnOpen = false }: { id: number; onchange?: () => void; markOnOpen?: boolean } = $props();

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

  // Mark it read once a person opened it (once per message: "Mark unread"
  // is not undone).
  let autoMarked: number | null = null;
  $effect(() => {
    if (!markOnOpen || !message || message.id !== id || !delivery || autoMarked === id) return;
    autoMarked = id;
    if (!delivery.read_at) void run(() => markRead(id));
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
  export function doMarkUnread() {
    if (message && delivery?.read_at && !delivery.acked_at) void run(() => markUnread(id));
  }
  export function doAck() {
    if (message?.ack_required && delivery && !delivery.acked_at) void run(() => ack(id));
  }
  /** Fetch the delivery state again (after the list changed it). */
  export function refresh() {
    const mid = id;
    get<MessageResponse>(`/v1/messages/${enc(String(mid))}?peek=1`)
      .then((r) => {
        if (mid === id) delivery = r.delivery;
      })
      .catch(() => undefined);
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
  {@const tags = subjectRequestTags(message.subject)}
  {#if tags.length}<RequestStrip nums={tags} />{/if}
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
      {#if delivery?.read_at && !delivery.acked_at}
        <button class="small" disabled={busy} onclick={doMarkUnread}>{t('mail.markUnread')} <kbd>u</kbd></button>
      {:else if delivery}
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
