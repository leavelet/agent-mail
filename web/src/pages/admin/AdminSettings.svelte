<script lang="ts">
  import { get, patch } from '../../lib/api';
  import { t } from '../../lib/ui.svelte';
  import { store } from '../../lib/store.svelte';
  import * as notify from '../../lib/notify';
  import type { OrgInfo } from '../../lib/types';
  import DataNotice from '../../components/DataNotice.svelte';

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

  // Token rotation.
  let rotated = $state<string | null>(null);
  let tokenError = $state<string | null>(null);
  async function rotate() {
    tokenError = null;
    try {
      rotated = await store.rotateToken();
    } catch (err) {
      tokenError = err instanceof Error ? err.message : String(err);
    }
  }

  // Browser notifications.
  let notifyOn = $state(notify.enabled());
  let perm = $state<notify.Permission>(notify.permission());
  async function toggleNotify(e: Event) {
    const want = (e.currentTarget as HTMLInputElement).checked;
    if (want) {
      const ok = await notify.requestPermission();
      perm = notify.permission();
      notifyOn = ok;
      notify.setEnabled(ok);
    } else {
      notifyOn = false;
      notify.setEnabled(false);
    }
  }

  // Deleting the org.
  let confirmName = $state('');
  let deleteError = $state<string | null>(null);
  async function deleteOrg(e: SubmitEvent) {
    e.preventDefault();
    if (!org || confirmName !== org.name) return;
    deleteError = null;
    try {
      await store.deleteOrg(confirmName);
    } catch (err) {
      deleteError = err instanceof Error ? err.message : String(err);
    }
  }

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

  <section class="card form" aria-labelledby="s-token">
    <h2 id="s-token">{t('settings.token')}</h2>
    <p class="faint hint0">{t('settings.rotateHint')}</p>
    <div class="row"><button onclick={rotate}>{t('settings.rotate')}</button></div>
    {#if rotated}
      <div class="fresh" role="status">
        <p>{t('settings.rotated')}</p>
        <code class="tok" data-testid="rotated-token">{rotated}</code>
      </div>
    {/if}
    {#if tokenError}<p class="error" role="alert">{tokenError}</p>{/if}
  </section>

  <section class="card form" aria-labelledby="s-notify" data-testid="notify-settings">
    <h2 id="s-notify">{t('settings.notify')}</h2>
    <p class="faint hint0">{t('settings.notifyHint')}</p>
    {#if perm === 'unsupported'}
      <p class="faint">{t('settings.notifyUnsupported')}</p>
    {:else}
      <label class="check"><input type="checkbox" checked={notifyOn} onchange={toggleNotify} disabled={perm === 'denied'} />
        <span>{t('settings.notifyOn')}</span></label>
      {#if perm === 'denied'}<p class="error">{t('settings.notifyDenied')}</p>{:else if notifyOn}<p class="ok">{t('settings.notifyTest')}</p>{/if}
    {/if}
  </section>

  <section class="card form"><DataNotice /></section>

  {#if org}
    <form class="card form danger-zone" aria-labelledby="s-delete" onsubmit={deleteOrg} data-testid="delete-org">
      <h2 id="s-delete">{t('settings.danger')}</h2>
      <p>{t('settings.deleteHint', { org: org.name })}</p>
      <label><span>{t('settings.deleteType')}</span><input placeholder={org.name} bind:value={confirmName} autocomplete="off" spellcheck="false" /></label>
      <div class="row"><button class="danger" type="submit" disabled={confirmName !== org.name}>{t('settings.deleteButton')}</button></div>
      {#if deleteError}<p class="error" role="alert">{deleteError}</p>{/if}
    </form>
  {/if}
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
  section,
  .danger-zone {
    margin-top: 14px;
  }
  h2 {
    font-size: 14px;
    margin: 0;
  }
  .hint0 {
    margin: 0;
    font-size: 12px;
  }
  label.check {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  label.check span {
    color: var(--text);
  }
  .fresh {
    padding: 8px 10px;
    border: 1px solid var(--warn);
    background: var(--warn-weak);
    border-radius: 4px;
  }
  .fresh p {
    margin: 0 0 6px;
  }
  .tok {
    display: block;
    user-select: all;
    word-break: break-all;
  }
  .danger-zone {
    border-color: var(--danger);
  }
  .danger-zone p {
    margin: 0;
  }
</style>
