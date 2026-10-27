<script lang="ts">
  // An input for a document: `d12`, a slug, or title words, with the
  // documents whose title matches offered below it (`GET /v1/docs/find`).
  import Vis from './Vis.svelte';
  import { findDocs } from '../lib/docops';
  import { docId, parseDocNum } from '../lib/docs';
  import { t } from '../lib/ui.svelte';
  import type { Doc } from '../lib/types';

  let { value = $bindable(''), exclude, label }: { value?: string; exclude?: number; label: string } = $props();

  let hits = $state.raw<Doc[]>([]);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let gen = 0;

  function onInput() {
    clearTimeout(timer);
    const q = value.trim();
    if (!q || parseDocNum(q)) {
      hits = [];
      return;
    }
    timer = setTimeout(async () => {
      const g = ++gen;
      try {
        const r = await findDocs(q, { limit: 8 });
        if (g === gen) hits = r.filter((d) => d.num !== exclude);
      } catch {
        if (g === gen) hits = [];
      }
    }, 250);
  }

  function pick(d: Doc) {
    value = docId(d.num);
    hits = [];
  }
</script>

<span class="picker">
  <input class="mono" placeholder={t('doc.pickerHint')} aria-label={label} bind:value oninput={onInput} />
  {#if hits.length}
    <ul class="hits card" role="listbox" aria-label={label}>
      {#each hits as d (d.num)}
        <li role="option" aria-selected="false">
          <button type="button" class="ghost small" onclick={() => pick(d)}><span class="mono">{docId(d.num)}</span> <Vis text={d.title} /> <span class="faint">{d.kind}</span></button>
        </li>
      {/each}
    </ul>
  {/if}
</span>

<style>
  .picker {
    position: relative;
    display: inline-flex;
    flex-direction: column;
    min-width: 0;
  }
  .hits {
    list-style: none;
    margin: 2px 0 0;
    padding: 4px;
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 5;
    min-width: 260px;
    max-width: min(480px, 90vw);
    box-shadow: 0 4px 14px rgb(0 0 0 / 0.15);
  }
  .hits button {
    width: 100%;
    text-align: left;
    white-space: normal;
  }
</style>
