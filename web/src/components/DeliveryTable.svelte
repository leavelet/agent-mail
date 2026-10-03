<script lang="ts">
  import { get, enc } from '../lib/api';
  import type { StatusResponse } from '../lib/types';
  import { deliveryState, deliveryStep, reachedBy, viaLabel } from '../lib/delivery';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { shortTime } from '../lib/time';

  let { id, message }: { id: number; message?: { to: string[]; cc: string[] } } = $props();
  const isList = (a: string) => store.lists.some((l) => l.name === a);
  let data = $state.raw<StatusResponse | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    const mid = id;
    get<StatusResponse>(`/v1/messages/${enc(String(mid))}/status`)
      .then((r) => (data = r))
      .catch((e) => (error = e instanceof Error ? e.message : String(e)));
  });
</script>

<div class="dlv" data-testid="delivery">
  {#if error}
    <p class="error">{error}</p>
  {:else if !data}
    <p class="faint">{t('app.loading')}</p>
  {:else}
    <table>
      <thead><tr><th>{t('dlv.recipient')}</th><th>{t('dlv.class')}</th><th>{t('dlv.state')}</th><th>{t('dlv.times')}</th></tr></thead>
      <tbody>
        {#each data.recipients as r}
          {@const st = deliveryState(r)}
          {@const reach = reachedBy(r.address, r.class, message ?? { to: [], cc: [] }, isList)}
          <tr>
            <td class="mono">{r.address}</td>
            <td class="faint">{reach.cls}{reach.via ? ` · ${t('mail.viaList', { list: reach.via })}` : ''}{r.mode !== 'immediate' ? ` · ${r.mode}` : ''}</td>
            <td>
              <span class="steps" aria-label={t(`dlv.${st}`)}>
                {#each [0, 1, 2, 3] as i}<span class="step" class:on={i <= deliveryStep(st)}></span>{/each}
              </span>
              <span class="st st-{st}" data-dstate={st}>{t(`dlv.${st}`)}</span>
            </td>
            <td class="faint"><div class="times">
              {#if r.notified_at}<span>{t('dlv.notified')} {shortTime(r.notified_at, ui.now, ui.lang)}{#if r.notified_via} · {t('dlv.via', { via: viaLabel(r.notified_via) })}{/if}</span>{/if}
              {#if r.read_at}<span>{t('dlv.read')} {shortTime(r.read_at, ui.now, ui.lang)}</span>{/if}
              {#if r.acked_at}<span>{t('dlv.acked')} {shortTime(r.acked_at, ui.now, ui.lang)}</span>{/if}
            </div></td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .dlv {
    margin-top: 6px;
    border: 1px solid var(--border);
    border-radius: 5px;
    overflow: hidden;
  }
  th,
  td {
    padding: 3px 8px;
    font-size: 12px;
  }
  .steps {
    display: inline-flex;
    gap: 2px;
    margin-right: 6px;
    vertical-align: middle;
  }
  .step {
    width: 10px;
    height: 4px;
    border-radius: 2px;
    background: var(--surface-3);
  }
  .step.on {
    background: var(--accent);
  }
  .st {
    font-weight: 600;
  }
  .st-acked {
    color: var(--ok);
  }
  .st-queued {
    color: var(--faint);
  }
  .times {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 10px;
  }
  p {
    margin: 6px 8px;
  }
</style>
