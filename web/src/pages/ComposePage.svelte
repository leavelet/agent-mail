<script lang="ts">
  import { get, post, enc, ApiError } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { go } from '../lib/router.svelte';
  import { buildSend, replySubject, type ComposeError, type MetaRow } from '../lib/compose';
  import { ack, markRead } from '../lib/mailops';
  import { activeOrgs, peerCandidates } from '../lib/openings';
  import type { Card, MessageResponse, SendResponse, Message, Delivery, ThreadResponse } from '../lib/types';
  import AddressInput from '../components/AddressInput.svelte';
  import MessageCard from '../components/MessageCard.svelte';
  import SendReceipt from '../components/SendReceipt.svelte';

  let { query }: { query: URLSearchParams } = $props();

  let to = $state('');
  let cc = $state('');
  let subject = $state('');
  let body = $state('');
  let urgent = $state(false);
  let ackReq = $state(false);
  let meta = $state<MetaRow[]>([]);
  let replyTo = $state.raw<Message | null>(null);
  let replyDelivery = $state.raw<Delivery | undefined>(undefined);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let bodyEl: HTMLTextAreaElement | undefined = $state();
  /** The answer to the last send: shown instead of the form. */
  let receipt = $state.raw<SendResponse | null>(null);
  /** Another org's seats that wrote in the thread being answered. */
  let participants = $state.raw<Card[]>([]);
  /** Some recipients were rejected as unreachable. */
  let unreachable = $state(false);

  // Addresses of other orgs to offer: only what they opened to this org,
  // and the participants of the thread being answered.
  $effect(() => {
    void store.links;
    void store.loadPeers();
  });
  const external = $derived(peerCandidates(store.peerCards, activeOrgs(store.links), participants));

  $effect(() => {
    const reply = query.get('reply');
    const all = query.get('all') === '1';
    to = query.get('to') ?? '';
    cc = '';
    receipt = null;
    subject = query.get('subject') ?? '';
    replyTo = null;
    participants = [];
    if (!reply) return;
    get<MessageResponse>(`/v1/messages/${enc(reply)}?peek=1`)
      .then((r) => {
        const m = r.message;
        replyTo = m;
        replyDelivery = r.delivery;
        get<ThreadResponse>(`/v1/threads/${enc(String(m.thread_id))}`)
          .then((th) => (participants = th.participants ?? []))
          .catch(() => undefined);
        const me = store.me?.seat;
        const toList = all ? [m.from, ...m.to.filter((a) => a !== me && a !== m.from)] : [m.from];
        to = toList.join(', ');
        cc = all ? m.cc.filter((a) => a !== me).join(', ') : '';
        subject = replySubject(m.subject);
        if (m.meta) meta = Object.entries(m.meta).filter(([k]) => k === 'repo' || k === 'branch').map(([key, value]) => ({ key, value }));
        setTimeout(() => bodyEl?.focus(), 0);
      })
      .catch((e) => (error = e instanceof Error ? e.message : String(e)));
  });

  function errText(e: ComposeError): string {
    switch (e.kind) {
      case 'noTo':
        return t('compose.err.noTo');
      case 'addr':
        return t('compose.err.addr', { a: e.a });
      case 'metaKey':
        return t('compose.err.metaKey', { k: e.k });
      case 'metaValue':
        return t('compose.err.metaValue', { k: e.k });
      case 'metaDup':
        return t('compose.err.metaDup', { k: e.k });
      case 'subject':
        return t('compose.err.subject');
    }
  }

  /** Start a new message after a receipt. */
  function again() {
    receipt = null;
    to = '';
    cc = '';
    subject = '';
    body = '';
    urgent = false;
    ackReq = false;
    meta = [];
    replyTo = null;
    if (query.get('reply') || query.get('to') || query.get('subject')) go(['compose']);
  }

  async function send(e?: SubmitEvent) {
    e?.preventDefault();
    error = null;
    unreachable = false;
    const { req, error: ce } = buildSend({ to, cc, subject, body, urgent, ack: ackReq, meta, inReplyTo: replyTo?.id });
    if (ce || !req) {
      error = ce ? errText(ce) : null;
      return;
    }
    busy = true;
    try {
      const r = await post<SendResponse>('/v1/messages', req);
      if (replyTo && replyDelivery) {
        if (replyTo.ack_required && !replyDelivery.acked_at) await ack(replyTo.id).catch(() => undefined);
        else if (!replyDelivery.read_at) await markRead(replyTo.id).catch(() => undefined);
      }
      void store.mailChanged();
      receipt = r;
    } catch (err) {
      if (err instanceof ApiError && err.rejected.length) {
        error = t('compose.rejected', { list: err.rejected.map((x) => `${x.address} (${x.reason})`).join(', ') });
        unreachable = err.rejected.some((x) => x.reason === 'unreachable');
      } else error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<div class="page compose">
  <div class="page-head"><h1>{replyTo ? t('compose.replyTitle') : t('compose.title')}</h1></div>
  {#if receipt}
    <SendReceipt r={receipt} onagain={again} />
  {:else}
  <form class="card form" onsubmit={send}>
    <div class="field"><label for="c-to">{t('compose.to')}</label><AddressInput id="c-to" label={t('compose.to')} {external} bind:value={to} /></div>
    <div class="field"><label for="c-cc">{t('compose.cc')}</label><AddressInput id="c-cc" label={t('compose.cc')} {external} bind:value={cc} /></div>
    <p class="hint faint">{t('compose.addrHint')}</p>
    <div class="field"><label for="c-subj">{t('compose.subject')}</label><input id="c-subj" type="text" maxlength="300" bind:value={subject} /></div>
    <textarea
      id="c-body"
      aria-label={t('compose.body')}
      rows="12"
      bind:value={body}
      bind:this={bodyEl}
      placeholder={t('compose.body')}
      onkeydown={(e) => {
        if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) void send();
      }}
    ></textarea>
    <div class="row wrap opts">
      <label class="check"><input type="checkbox" bind:checked={urgent} /> {t('compose.urgent')}</label>
      <label class="check"><input type="checkbox" bind:checked={ackReq} /> {t('compose.ack')}</label>
    </div>
    <fieldset>
      <legend>{t('compose.meta')}</legend>
      {#each meta as row, i}
        <div class="row metarow">
          <input class="mono" aria-label={t('compose.key')} placeholder={t('compose.key')} bind:value={row.key} size="12" />
          <input class="mono val" aria-label={t('compose.value')} placeholder={t('compose.value')} bind:value={row.value} />
          <button type="button" class="small ghost" onclick={() => (meta = meta.filter((_, j) => j !== i))}>{t('compose.remove')}</button>
        </div>
      {/each}
      <div class="row">
        <button type="button" class="small" onclick={() => (meta = [...meta, { key: '', value: '' }])}>+ {t('compose.addMeta')}</button>
        {#each ['repo', 'branch', 'commit'] as k}
          {#if !meta.some((m) => m.key === k)}
            <button type="button" class="small ghost mono" onclick={() => (meta = [...meta, { key: k, value: '' }])}>+ {k}</button>
          {/if}
        {/each}
      </div>
    </fieldset>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if unreachable}<p class="faint" data-testid="unreachable-hint">{t('compose.unreachable')}</p>{/if}
    <div class="row">
      <span class="spacer"></span>
      <button type="button" onclick={() => history.back()}>{t('compose.cancel')}</button>
      <button class="primary" type="submit" disabled={busy}>{busy ? t('compose.sending') : t('compose.send')} <kbd>Ctrl ↵</kbd></button>
    </div>
  </form>
  {/if}
  {#if replyTo}
    <div class="quoted">
      <MessageCard message={replyTo} />
    </div>
  {/if}
</div>

<style>
  .compose {
    max-width: 860px;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 14px 16px;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .field label {
    width: 64px;
    color: var(--muted);
    flex: none;
  }
  .field input {
    flex: 1;
  }
  .hint {
    margin: -4px 0 0 74px;
    font-size: 11.5px;
  }
  textarea {
    width: 100%;
    font-family: var(--font);
  }
  .check {
    display: inline-flex;
    gap: 5px;
    align-items: center;
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 6px 10px 8px;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  legend {
    color: var(--muted);
    padding: 0 4px;
  }
  .metarow .val {
    flex: 1;
  }
  kbd {
    margin-left: 4px;
    font-size: 10px;
    opacity: 0.8;
    background: transparent;
    color: inherit;
  }
  .quoted {
    margin-top: 14px;
    opacity: 0.85;
  }
</style>
