<script lang="ts">
  // One document named in a list: its id, title, kind and status, with the
  // stale mark.
  import Vis from './Vis.svelte';
  import { t } from '../lib/ui.svelte';
  import { docHref, docId, docStatusTone } from '../lib/docs';
  import type { DocRef } from '../lib/types';

  let { doc, anchor, showKind = true }: { doc: DocRef; anchor?: string; showKind?: boolean } = $props();
</script>

<span class="dref" data-doc={doc.num}>
  <a class="mono" href={docHref(doc.num, anchor)}>{docId(doc.num)}{#if anchor}#{anchor}{/if}</a>
  <a href={docHref(doc.num, anchor)}><Vis text={doc.title} /></a>
  {#if showKind}<span class="chip neutral">{doc.kind}</span>{/if}
  {#if doc.status !== 'active'}<span class="chip tone-{docStatusTone(doc.status)}">{t(`doc.status.${doc.status}`)}</span>{/if}
  {#if doc.stale}<span class="chip attn" title={t('doc.stale.long')}>{t('doc.stale')}</span>{/if}
</span>

<style>
  .dref {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    min-width: 0;
    overflow-wrap: anywhere;
  }
</style>
