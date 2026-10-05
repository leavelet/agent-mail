<script lang="ts">
  // One change of openings, previewed before it is applied: the server says
  // which org would see what more or less (`POST /v1/grants/preview`); the
  // change is made only when the user confirms.
  import Vis from './Vis.svelte';
  import { post, del, ApiError } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { changeLines, type ChangeLine, type Pending } from '../lib/openings';
  import type { GrantChange } from '../lib/types';

  let { change, onclose }: { change: Pending; onclose: (applied: boolean) => void } = $props();

  let lines = $state.raw<ChangeLine[] | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

  const target = $derived(change.add ? change.add.target : change.remove!.target);
  const audience = $derived(change.add ? (change.add.audience ?? 'linked') : change.remove!.audience);
  const aud = (a: string) => (a === 'linked' ? t('open.linked') : a);

  function message(e: unknown): string {
    if (e instanceof ApiError && e.code === 'no_contacts') return t('open.noContacts', { team: target });
    return e instanceof Error ? e.message : String(e);
  }

  $effect(() => {
    const c = change;
    lines = null;
    error = null;
    post<GrantChange>('/v1/grants/preview', c.add ? { add: [c.add], remove: [] } : { add: [], remove: [c.remove!.id] })
      .then((r) => (lines = changeLines(r.changes)))
      .catch((e) => (error = message(e)));
  });

  async function apply() {
    busy = true;
    error = null;
    try {
      if (change.add) await post<GrantChange>('/v1/grants', change.add);
      else await del<GrantChange>(`/v1/grants/${change.remove!.id}`);
      await store.openingsChanged();
      onclose(true);
    } catch (e) {
      error = message(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="preview" role="dialog" aria-label={t('open.preview')} data-testid="grant-preview">
  <strong>{t(change.add ? 'open.willOpen' : 'open.willClose', { target, audience: aud(audience) })}</strong>
  {#if error}
    <p class="error" role="alert">{error}</p>
  {:else if lines === null}
    <p class="faint">{t('open.previewing')}</p>
  {:else if lines.length === 0}
    <p data-testid="preview-none">{t('open.noChange')}</p>
  {:else}
    <ul data-testid="preview-lines">
      {#each lines as l}
        <li class={l.verb}>
          {t(l.verb === 'gains' ? 'open.gains' : 'open.loses', { org: l.org })}
          <span class="mono"><Vis text={l.names.join(', ')} /></span>
        </li>
      {/each}
    </ul>
  {/if}
  <div class="row">
    <button class="small {change.add ? 'primary' : 'danger'}" disabled={busy || lines === null || !!error} onclick={apply} data-testid="preview-apply">
      {t(change.add ? 'open.applyOpen' : 'open.applyClose')}
    </button>
    <button class="small" onclick={() => onclose(false)}>{t('admin.cancel')}</button>
  </div>
</div>

<style>
  .preview {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 8px 10px;
    border: 1px solid var(--warn);
    background: var(--warn-weak);
    border-radius: var(--radius);
  }
  p,
  ul {
    margin: 0;
  }
  ul {
    padding-left: 18px;
  }
  li.gains::marker {
    content: '+ ';
    color: var(--ok);
  }
  li.loses::marker {
    content: '− ';
    color: var(--danger);
  }
</style>
