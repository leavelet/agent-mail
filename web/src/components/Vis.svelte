<script lang="ts">
  // Text written by someone else, with characters that are not shown
  // (bidirectional controls, zero-width characters, variation selectors,
  // control characters) rendered as visible escapes such as `\u{202e}`.
  import { segments } from '../lib/visible';
  import { t } from '../lib/ui.svelte';

  let { text }: { text: string } = $props();
  const parts = $derived(segments(text));
</script>

{#each parts as p, i (i)}{#if p.escaped}<span class="esc" title={t('mail.hiddenChars')}>{p.text}</span>{:else}{p.text}{/if}{/each}

<style>
  .esc {
    font-family: var(--mono);
    font-size: 0.85em;
    color: var(--danger);
    background: var(--danger-weak);
    border-radius: 3px;
    padding: 0 2px;
  }
</style>
