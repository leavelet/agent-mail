<script lang="ts">
  import { get, post, enc } from '../lib/api';
  import type { StatusResponse, WithdrawResponse } from '../lib/types';
  import { canWithdraw, deliveryState, deliveryStep, reachedBy, viaLabel } from '../lib/delivery';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { shortTime } from '../lib/time';

  let { id, message }: { id: number; message?: { to: string[]; cc: string[] } } = $props();
  const isList = (a: string) => store.lists.some((l) => l.name === a);
  let data = $state.raw<StatusResponse | null>(null);
  let error = $state<string | null>(null);

  function load(mid: number) {
    get<StatusResponse>(`/v1/messages/${enc(String(mid))}/status`)
      .then((r) => (data = r))
      .catch((e) => (error = e instanceof Error ? e.message : String(e)));
  }
  $effect(() => load(id));

  let busy = $state<string | null>(null);
  async function withdraw(seat: string) {
    busy = seat;
    error = null;
    try {
      await post<WithdrawResponse>(`/v1/messages/${enc(String(id))}/deliveries/${enc(seat)}/withdraw`);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = null;
      load(id);
    }
  }
</script>

<div class="dlv" data-testid="delivery">
  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
  {#if !data && !error}
    <p class="faint">{t('app.loading')}</p>
  {:else if data}
    <table>
      <thead><tr><th>{t('dlv.recipient')}</th><th>{t('dlv.class')}</th><th>{t('dlv.state')}</th><th>{t('dlv.times')}</th></tr></thead>
      <tbody>
        {#each data.recipients as r}
          {@const st = deliveryState(r)}
          {@const reach = reachedBy(r.address, r.class, message ?? { to: [], cc: [] }, isList)}
          <tr>
            <td class="mono">{r.address}</td>
            <td class="faint">{reach.cls}{reach.via ? ` · ${t('mail.viaList', { list: reach.via })}` : ''}{r.mode !== 'immediate' && !r.withdrawn_at ? ` · ${r.mode}` : ''}</td>
            <td>
              <span class="steps" aria-label={t(`dlv.${st}`)}>
                {#each [0, 1, 2, 3] as i}<span class="step" class:on={i <= deliveryStep(st)}></span>{/each}
              </span>
              <span class="st st-{st}" data-dstate={st} title={t(`dlv.${st}.long`)}>{t(`dlv.${st}`)}</span>
            </td>
            <td class="faint"><div class="times">
              {#if r.members}<span data-testid="dlv-members">{t('dlv.members', { seats: r.members.seats, read: r.members.read, acked: r.members.acked })}</span>{/if}
              {#if r.notified_at}<span>{t('dlv.notified')} {shortTime(r.notified_at, ui.now, ui.lang)}{#if r.notified_via} · {t('dlv.via', { via: viaLabel(r.notified_via) })}{/if}</span>{/if}
              {#if r.read_at}<span>{t('dlv.read')} {shortTime(r.read_at, ui.now, ui.lang)}</span>{/if}
              {#if r.acked_at}<span>{t('dlv.acked')} {shortTime(r.acked_at, ui.now, ui.lang)}</span>{/if}
              {#if r.withdrawn_at}<span>{t('dlv.withdrawn')} {shortTime(r.withdrawn_at, ui.now, ui.lang)}</span>{/if}
              {#if canWithdraw(r.address, r)}<button class="small" data-testid="withdraw" disabled={busy !== null} title={t('dlv.withdraw.long')} onclick={() => withdraw(r.address)}>{t('dlv.withdraw')}</button>{/if}
            </div></td>
          </tr>
        {/each}
      </tbody>
    </table>
    <dl class="legend" data-testid="dlv-legend">
      {#each ['notified', 'read', 'acked', 'withdrawn'] as const as k}
        <div><dt>{t(`dlv.${k}`)}</dt><dd>{t(`dlv.${k}.long`)}</dd></div>
      {/each}
    </dl>
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
  .st-queued,
  .st-withdrawn {
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
  .legend {
    margin: 0;
    padding: 5px 8px;
    border-top: 1px solid var(--border);
    background: var(--surface-2);
    font-size: 11.5px;
    color: var(--muted);
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .legend div {
    display: flex;
    gap: 6px;
  }
  .legend dt {
    font-weight: 600;
    min-width: 72px;
  }
  .legend dd {
    margin: 0;
  }
</style>
