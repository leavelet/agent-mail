<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';

  let { compact = false }: { compact?: boolean } = $props();
  const days = $derived(store.info?.retention_days);
</script>

<section class="notice" class:compact aria-labelledby="data-title">
  <h2 id="data-title">{t('data.title')}</h2>
  <ul>
    <li>{t('data.stores')}</li>
    <li>{t('data.notStored')}</li>
    <li>{t('data.operator')}</li>
    <li>{days !== undefined ? t('data.retention', { days }) : t('data.noRetention')}</li>
    <li>{t('data.deleteMail')}</li>
    <li>{t('data.delete')}</li>
  </ul>
</section>

<style>
  .notice {
    font-size: 13px;
    color: var(--muted);
  }
  h2 {
    font-size: 13px;
    margin: 0 0 4px;
    color: var(--text);
  }
  ul {
    margin: 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .compact {
    font-size: 12px;
  }
</style>
