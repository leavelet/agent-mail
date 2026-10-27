<script lang="ts">
  // A document body rendered from Markdown (sanitized; see lib/markdown),
  // with the server's section anchors on its headings. `anchor` scrolls to
  // its section when it changes.
  import { tick } from 'svelte';
  import { renderDoc } from '../lib/markdown';
  import { t } from '../lib/ui.svelte';
  import type { DocSection } from '../lib/types';

  let { body, sections, num, anchor = '' }: { body: string; sections: DocSection[]; num: number | null; anchor?: string } = $props();

  const rendered = $derived(renderDoc(body, sections, num));
  let el: HTMLDivElement | undefined = $state();
  let missing = $state(false);

  /** The body's nodes, sanitized by renderDoc (DOMPurify and a second
   * pass), go into the element as nodes, never as HTML text. */
  function content(fragment: DocumentFragment) {
    return (node: HTMLElement) => {
      node.replaceChildren(fragment.cloneNode(true));
    };
  }

  $effect(() => {
    const a = anchor;
    void rendered;
    missing = false;
    if (!a) return;
    void tick().then(() => {
      const target = el?.querySelector(`[data-anchor="${CSS.escape(a)}"]`);
      if (target) {
        target.scrollIntoView({ block: 'start' });
        target.classList.add('flash');
        setTimeout(() => target.classList.remove('flash'), 1600);
      } else missing = true;
    });
  });
</script>

{#if missing}<p class="faint missing" role="status">{t('doc.noSection', { anchor })}</p>{/if}
{#if rendered.front !== null}
  <details class="front">
    <summary>{t('doc.frontMatter')}</summary>
    <pre>{rendered.front}</pre>
  </details>
{/if}
<div class="md" bind:this={el} data-testid="doc-body" {@attach content(rendered.fragment)}></div>

<style>
  .md {
    overflow-wrap: anywhere;
    line-height: 1.55;
    font-size: 14px;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4),
  .md :global(h5),
  .md :global(h6) {
    margin: 1.1em 0 0.4em;
    scroll-margin-top: 8px;
    border-radius: 4px;
    transition: background 0.4s;
  }
  .md :global(h1) {
    font-size: 20px;
  }
  .md :global(h2) {
    font-size: 17px;
  }
  .md :global(h3) {
    font-size: 15px;
  }
  .md :global(h4),
  .md :global(h5),
  .md :global(h6) {
    font-size: 14px;
  }
  .md :global(:first-child) {
    margin-top: 0;
  }
  .md :global(.flash) {
    background: var(--accent-weak);
  }
  .md :global(.hlink) {
    margin-left: 6px;
    color: var(--faint);
    opacity: 0;
    font-weight: 400;
  }
  .md :global(h1:hover .hlink),
  .md :global(h2:hover .hlink),
  .md :global(h3:hover .hlink),
  .md :global(h4:hover .hlink),
  .md :global(h5:hover .hlink),
  .md :global(h6:hover .hlink) {
    opacity: 1;
  }
  .md :global(p),
  .md :global(ul),
  .md :global(ol),
  .md :global(blockquote),
  .md :global(table) {
    margin: 0 0 0.7em;
  }
  .md :global(ul),
  .md :global(ol) {
    padding-left: 1.5em;
  }
  .md :global(pre) {
    padding: 8px 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: auto;
    white-space: pre;
  }
  .md :global(code) {
    font-size: 0.92em;
  }
  .md :global(:not(pre) > code) {
    padding: 0 3px;
    background: var(--surface-2);
    border-radius: 3px;
  }
  .md :global(blockquote) {
    padding: 2px 10px;
    border-left: 3px solid var(--border-strong);
    color: var(--muted);
  }
  .md :global(table) {
    display: block;
    overflow-x: auto;
    border-collapse: collapse;
  }
  .md :global(th),
  .md :global(td) {
    border: 1px solid var(--border);
    padding: 3px 8px;
  }
  .md :global(hr) {
    border: 0;
    border-top: 1px solid var(--border);
  }
  .md :global(.deadlink) {
    text-decoration: underline dotted;
  }
  .front {
    margin-bottom: 8px;
  }
  .front summary {
    cursor: pointer;
    color: var(--muted);
    font-size: 12px;
  }
  .front pre {
    margin: 4px 0 0;
    padding: 6px 10px;
    background: var(--surface-2);
    border-radius: var(--radius);
  }
  .missing {
    margin: 0 0 6px;
  }
</style>
