<script lang="ts">
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { currentToken, completeToken, suggest, type Suggestion } from '../lib/compose';

  let { value = $bindable(''), id, label }: { value: string; id: string; label: string } = $props();

  let focused = $state(false);
  let active = $state(0);
  const token = $derived(currentToken(value));
  const items = $derived<Suggestion[]>(
    focused && token
      ? suggest(
          token,
          store.seats.filter((s) => s.kind === 'agent').map((s) => ({ name: s.name, detail: [s.harness, s.role?.split('\n', 1)[0]].filter(Boolean).join(' · ') })),
          store.lists.map((l) => l.name),
          store.links.filter((l) => l.state === 'active').map((l) => l.org),
        )
      : [],
  );

  function pick(s: Suggestion) {
    value = completeToken(value, s.address);
    active = 0;
  }

  function onkeydown(e: KeyboardEvent) {
    if (!items.length) return;
    if (e.key === 'ArrowDown') {
      active = (active + 1) % items.length;
      e.preventDefault();
    } else if (e.key === 'ArrowUp') {
      active = (active - 1 + items.length) % items.length;
      e.preventDefault();
    } else if (e.key === 'Enter' || e.key === 'Tab') {
      if (items[active] && items[active].address !== token) {
        pick(items[active]);
        e.preventDefault();
      }
    } else if (e.key === 'Escape') {
      focused = false;
    }
  }
</script>

<div class="ai">
  <input
    {id}
    class="mono"
    type="text"
    autocomplete="off"
    spellcheck="false"
    aria-label={label}
    role="combobox"
    aria-autocomplete="list"
    aria-expanded={items.length > 0}
    aria-controls="{id}-list"
    bind:value
    onfocus={() => (focused = true)}
    onblur={() => setTimeout(() => (focused = false), 150)}
    oninput={() => (active = 0)}
    {onkeydown}
  />
  {#if items.length}
    <ul class="sugg card" id="{id}-list" role="listbox">
      {#each items as s, i}
        <li role="option" aria-selected={i === active} class:active={i === active}>
          <button type="button" tabindex="-1" onmousedown={(e) => e.preventDefault()} onclick={() => pick(s)}>
            <span class="mono addr">{s.address}</span>
            <span class="kind">{t(`compose.sugg.${s.kind}`)}</span>
            {#if s.detail}<span class="faint detail">{s.detail}</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .ai {
    position: relative;
    flex: 1;
  }
  input {
    width: 100%;
  }
  .sugg {
    position: absolute;
    z-index: 20;
    left: 0;
    right: 0;
    top: calc(100% + 2px);
    margin: 0;
    padding: 3px;
    list-style: none;
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.18);
    max-height: 280px;
    overflow-y: auto;
  }
  li button {
    width: 100%;
    display: flex;
    gap: 8px;
    align-items: baseline;
    border: none;
    background: transparent;
    text-align: left;
    padding: 3px 8px;
    border-radius: 4px;
  }
  li.active button {
    background: var(--accent-weak);
  }
  .addr {
    font-weight: 600;
  }
  .kind {
    font-size: 11px;
    color: var(--muted);
  }
  .detail {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
  }
</style>
