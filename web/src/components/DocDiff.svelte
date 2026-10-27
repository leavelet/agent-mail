<script lang="ts">
  // A unified diff with added and removed lines marked.
  import Vis from './Vis.svelte';
  import { diffLines } from '../lib/docs';
  import { t } from '../lib/ui.svelte';

  let { diff }: { diff: string } = $props();
  const lines = $derived(diffLines(diff));
</script>

{#if lines.length}
  <pre class="diff" data-testid="doc-diff">{#each lines as l, i (i)}<span class="l {l.kind}"><Vis text={l.text} /></span>{/each}</pre>
{:else}
  <p class="faint">{t('doc.diff.none')}</p>
{/if}

<style>
  .diff {
    margin: 0;
    padding: 6px 0;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: auto;
    max-height: 480px;
    font-size: 12px;
    white-space: pre;
  }
  .l {
    display: block;
    min-height: 1.4em;
    min-width: 100%;
    padding: 0 10px;
  }
  .add {
    background: var(--ok-weak);
    color: var(--ok);
  }
  .del {
    background: var(--danger-weak);
    color: var(--danger);
  }
  .hunk {
    color: var(--notice);
  }
  .head {
    color: var(--muted);
    font-weight: 600;
  }
</style>
