<script lang="ts">
  import { get, patch } from '../../lib/api';
  import { t } from '../../lib/ui.svelte';
  import type { OrgInfo } from '../../lib/types';

  let org = $state.raw<OrgInfo | null>(null);
  let rate = $state(0);
  let burst = $state(0);
  let maxRecipients = $state(0);
  let depth = $state(0);
  let ackTimeout = $state(15);
  let focusMax = $state(240);
  let digestInterval = $state(30);
  let webhook = $state('');
  let msg = $state<string | null>(null);
  let error = $state<string | null>(null);

  function fill(o: OrgInfo) {
    org = o;
    rate = o.send_rate_per_min;
    burst = o.send_burst;
    maxRecipients = o.max_recipients;
    depth = o.max_thread_depth;
    ackTimeout = o.ack_timeout_min;
    focusMax = o.focus_max_min;
    digestInterval = o.digest_interval_min;
    webhook = o.webhook_url ?? '';
  }

  $effect(() => {
    get<OrgInfo>('/v1/org')
      .then(fill)
      .catch((e) => (error = e instanceof Error ? e.message : String(e)));
  });

  async function save(e: SubmitEvent) {
    e.preventDefault();
    msg = null;
    error = null;
    try {
      fill(
        await patch<OrgInfo>('/v1/org', {
          send_rate_per_min: rate,
          send_burst: burst,
          max_recipients: maxRecipients,
          max_thread_depth: depth,
          ack_timeout_min: ackTimeout,
          focus_max_min: focusMax,
          digest_interval_min: digestInterval,
          webhook_url: webhook.trim() || null,
        }),
      );
      msg = t('admin.saved');
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }
</script>

<div class="page">
  <div class="page-head"><h1>{t('nav.admin')} · {t('nav.settings')}</h1>{#if org}<span class="mono faint">{org.name}</span>{/if}</div>
  {#if org}
    <form class="card form" onsubmit={save}>
      <label><span>{t('admin.org.rate')}</span><input type="number" min="0" bind:value={rate} /></label>
      <label><span>{t('admin.org.burst')}</span><input type="number" min="1" bind:value={burst} /></label>
      <label><span>{t('admin.org.maxRecipients')}</span><input type="number" min="1" bind:value={maxRecipients} /></label>
      <label><span>{t('admin.org.depth')}</span><input type="number" min="1" bind:value={depth} /></label>
      <label><span>{t('admin.org.ackTimeout')}</span><input type="number" min="1" bind:value={ackTimeout} /></label>
      <label><span>{t('admin.org.focusMax')}</span><input type="number" min="1" bind:value={focusMax} /></label>
      <label><span>{t('admin.org.digestInterval')}</span><input type="number" min="1" bind:value={digestInterval} /></label>
      <label><span>{t('admin.org.webhook')}</span><input type="url" placeholder="https://…" bind:value={webhook} /></label>
      <p class="faint hint">{t('admin.org.webhookHint')}</p>
      <div class="row"><button class="primary" type="submit">{t('admin.save')}</button>{#if msg}<span class="ok">{msg}</span>{/if}</div>
      {#if error}<p class="error" role="alert">{error}</p>{/if}
    </form>
  {:else if error}<p class="error">{error}</p>{/if}
</div>

<style>
  .form {
    max-width: 620px;
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  label {
    display: grid;
    grid-template-columns: 260px 1fr;
    align-items: center;
    gap: 10px;
  }
  label span {
    color: var(--muted);
  }
  .hint {
    margin: -2px 0 0 270px;
    font-size: 12px;
  }
</style>
