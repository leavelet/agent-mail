<script lang="ts">
  import { t, ui, type Theme } from '../lib/ui.svelte';
  import type { Lang } from '../lib/i18n';

  const themes: Theme[] = ['system', 'light', 'dark'];
</script>

<div class="lt">
  <select aria-label={t('top.lang')} value={ui.lang} onchange={(e) => ui.setLang((e.currentTarget as HTMLSelectElement).value as Lang)}>
    <option value="en">English</option>
    <option value="zh">中文</option>
  </select>
  <select
    aria-label={t('top.theme')}
    value={ui.theme}
    onchange={(e) => ui.setTheme((e.currentTarget as HTMLSelectElement).value as Theme)}
  >
    {#each themes as th}
      <option value={th}>{t(`theme.${th}`)}</option>
    {/each}
  </select>
</div>

<style>
  .lt {
    display: flex;
    gap: 6px;
  }
  select {
    min-height: 24px;
    padding: 1px 4px;
    font-size: 12px;
  }
</style>
