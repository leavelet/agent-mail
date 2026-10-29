<script lang="ts">
  // The requests a message's subject names (`[r12]`), above the message:
  // the options as buttons that answer through the API while a request is
  // open, else its answer.
  import RequestCard from './RequestCard.svelte';
  import { store } from '../lib/store.svelte';
  import { getRequest } from '../lib/requestops';
  import type { Request } from '../lib/types';

  let { nums }: { nums: number[] } = $props();

  let reqs = $state.raw<Request[]>([]);
  let gen = 0;

  async function load(list: number[]) {
    const g = ++gen;
    const got = await Promise.all(list.slice(0, 3).map((n) => getRequest(n).then((d) => d.request).catch(() => null)));
    if (g === gen) reqs = got.filter((r): r is Request => r !== null);
  }

  // Load when the numbers change and on request events that concern them.
  let loaded = '';
  $effect(() => {
    void store.requestSeq;
    const changed = store.requestChanged;
    const list = nums;
    const key = list.join(',');
    if (key !== loaded || !changed || list.some((n) => changed.has(n))) void load(list);
    loaded = key;
  });
</script>

{#if reqs.length}
  <div class="strip" data-testid="request-strip">
    {#each reqs as r (r.num)}
      <RequestCard request={r} mode="strip" onresolved={(next) => (reqs = reqs.map((x) => (x.num === next.num ? next : x)))} />
    {/each}
  </div>
{/if}

<style>
  .strip {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 8px;
  }
</style>
