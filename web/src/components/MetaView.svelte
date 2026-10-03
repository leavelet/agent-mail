<script lang="ts">
  import { t } from '../lib/ui.svelte';

  let { meta }: { meta: Record<string, string> } = $props();
  const known = ['repo', 'branch', 'commit'] as const;
  const hasGit = $derived(known.some((k) => meta[k]));
  const rest = $derived(Object.entries(meta).filter(([k]) => !(known as readonly string[]).includes(k)));
</script>

<div class="meta">
  {#if hasGit}
    <div class="git" aria-label="git">
      {#if meta.repo}<span class="f"><span class="k">{t('meta.repo')}</span><span class="v mono">{meta.repo}</span></span>{/if}
      {#if meta.branch}<span class="f"><span class="k">{t('meta.branch')}</span><span class="v mono branch">⎇ {meta.branch}</span></span>{/if}
      {#if meta.commit}<span class="f"><span class="k">{t('meta.commit')}</span><span class="v mono commit" title={meta.commit}>{meta.commit.slice(0, 12)}</span></span>{/if}
    </div>
  {/if}
  {#if rest.length}
    <dl>
      {#each rest as [k, v]}
        <dt class="mono">{k}</dt>
        <dd class="mono">{v}</dd>
      {/each}
    </dl>
  {/if}
</div>

<style>
  .meta {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .git {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .f {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 5px;
    overflow: hidden;
    font-size: 12px;
  }
  .k {
    padding: 0 6px;
    background: var(--surface-2);
    color: var(--muted);
  }
  .v {
    padding: 0 6px;
  }
  .commit {
    color: var(--accent);
    font-weight: 600;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 1px 10px;
    margin: 0;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>
