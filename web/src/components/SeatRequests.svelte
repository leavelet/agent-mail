<script lang="ts">
  // A seat's open requests for the person (`GET /requests?seat=…`), and
  // the request it sleeps on when its node reports one.
  import Vis from './Vis.svelte';
  import { store } from '../lib/store.svelte';
  import { t, ui } from '../lib/ui.svelte';
  import { href } from '../lib/router.svelte';
  import { duration } from '../lib/time';
  import { parseTime } from '../lib/presence';
  import { requestId } from '../lib/requests';
  import { listRequests } from '../lib/requestops';
  import type { Request } from '../lib/types';

  let { seat, waiting }: { seat: string; waiting?: number } = $props();

  let rows = $state.raw<Request[]>([]);
  let gen = 0;

  async function load(name: string) {
    const g = ++gen;
    try {
      const r = await listRequests({ seat: name, state: 'open', limit: 50 });
      if (g === gen) rows = r.requests;
    } catch {
      // The seat page works without the list.
    }
  }

  $effect(() => {
    void store.requestSeq;
    void load(seat);
  });
</script>

{#if rows.length || waiting}
  <div class="sreqs" data-testid="seat-requests">
    {#if waiting}
      <p class="waiting" data-testid="seat-waiting">{t('req.waitingFor')} <a class="mono" href={href(['requests', requestId(waiting)])}>{requestId(waiting)}</a></p>
    {/if}
    {#if rows.length}
      <h3>{t('req.seatOpen')} <span class="count hot">{rows.length}</span></h3>
      <ul>
        {#each rows as r (r.num)}
          <li data-request={r.num}>
            <a class="mono" href={href(['requests', requestId(r.num)])}>{requestId(r.num)}</a>
            <a class="title" href={href(['requests', requestId(r.num)])}><Vis text={r.title} /></a>
            <span class="chip neutral">{t(`req.class.${r.class}`)}</span>
            <span class="faint">{t('req.age', { d: duration(ui.now - parseTime(r.created_at), ui.lang) })}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .sreqs {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .waiting {
    margin: 0;
    color: var(--warn);
    font-weight: 600;
  }
  h3 {
    font-size: 12px;
    color: var(--muted);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  li {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 8px;
    align-items: baseline;
  }
  .title {
    overflow-wrap: anywhere;
  }
</style>
