<script lang="ts">
  import { t, ui } from '../lib/ui.svelte';
  import type { Key } from '../lib/i18n';

  const rows: [string[], Key][] = [
    [['j', 'k', '↓', '↑'], 'keys.move'],
    [['Enter', 'o'], 'keys.open'],
    [['r'], 'keys.reply'],
    [['e'], 'keys.markRead'],
    [['u'], 'keys.markUnread'],
    [['a'], 'keys.ack'],
    [['c'], 'keys.compose'],
    [['/'], 'keys.search'],
    [['g n', 'g o', 'g i', 'g a'], 'keys.goto'],
    [['?'], 'keys.help'],
  ];
  let dialog: HTMLDivElement | undefined = $state();
  $effect(() => {
    dialog?.focus();
  });
</script>

<div class="backdrop" role="presentation" onclick={() => (ui.helpOpen = false)}>
  <div
    class="dialog card"
    role="dialog"
    aria-modal="true"
    aria-labelledby="help-title"
    tabindex="-1"
    bind:this={dialog}
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => {
      if (e.key === 'Escape') ui.helpOpen = false;
    }}
  >
    <div class="row"><h2 id="help-title">{t('keys.title')}</h2><span class="spacer"></span>
      <button class="ghost small" onclick={() => (ui.helpOpen = false)}>{t('keys.close')}</button></div>
    <table>
      <tbody>
        {#each rows as [ks, label]}
          <tr><td class="keys">{#each ks as k}<kbd>{k}</kbd> {/each}</td><td>{t(label)}</td></tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.35);
    display: grid;
    place-items: center;
    z-index: 50;
  }
  .dialog {
    width: min(460px, calc(100% - 32px));
    padding: 14px 16px;
    box-shadow: 0 12px 40px rgb(0 0 0 / 0.25);
  }
  .keys {
    white-space: nowrap;
    width: 1%;
  }
  td {
    border: none;
    padding: 4px 6px;
  }
</style>
