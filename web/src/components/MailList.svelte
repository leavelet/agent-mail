<script lang="ts">
  import Vis from './Vis.svelte';
  import { shortSubject } from '../lib/subject';
  import type { MailRow } from '../lib/mailrows';
  import { t, ui } from '../lib/ui.svelte';
  import { shortTime } from '../lib/time';
  import { deliverySummary } from '../lib/delivery';

  let {
    rows,
    selected,
    onselect,
    onopen,
    showTo = false,
    label,
  }: {
    rows: MailRow[];
    selected?: number;
    onselect?: (row: MailRow) => void;
    onopen?: (row: MailRow) => void;
    showTo?: boolean;
    label?: string;
  } = $props();
</script>

<div class="list" role="listbox" aria-label={label}>
  {#each rows as r (r.id)}
    <div
      class="mrow"
      class:unread={r.unread}
      class:sel={r.id === selected}
      role="option"
      aria-selected={r.id === selected}
      tabindex="-1"
      data-msg={r.id}
      onclick={() => onselect?.(r)}
      ondblclick={() => onopen?.(r)}
      onkeydown={(e) => {
        if (e.key === 'Enter') onopen?.(r);
      }}
    >
      <div class="who mono" title={showTo ? r.to.join(', ') : r.written_by ? `${r.from} (${t('mail.writtenBy', { user: r.written_by })})` : r.from}>
        {#if r.unread}<span class="udot" aria-label={t('mail.unread')}></span>{/if}
        {showTo ? `→ ${r.to.join(', ')}` : r.from}
        {#if !showTo && r.written_by}<span class="wb" title={t('mail.writtenBy.long')}>({t('mail.writtenBy', { user: r.written_by })})</span>{/if}
        {#if !showTo && r.source !== 'agent'}<span class="src src-{r.source}" title={t(`mail.source.${r.source}.long`)}>{t(`mail.source.${r.source}`)}</span>{/if}
      </div>
      <div class="subj">
        {#if r.priority === 'urgent'}<span class="chip urgent">{t('mail.urgent')}</span>{/if}
        {#if r.withdrawn}<span class="chip neutral" data-testid="withdrawn" title={t('dlv.withdrawn.inbox')}>{t('dlv.withdrawn')}</span>{/if}
        {#if r.ack_required}<span class="chip {r.acked ? 'ok' : 'ack'}">{r.acked ? t('mail.acked') : t('mail.ackReq')}</span>{/if}
        {#if r.via}<span class="chip neutral" title={t('mail.viaList.long', { list: r.via })}>{r.cls === 'cc' ? 'cc · ' : ''}{t('mail.viaList', { list: r.via })}</span>{:else if r.cls === 'cc'}<span class="chip neutral">cc</span>{/if}
        <span class="s">{#if r.subject}<Vis text={shortSubject(r.subject)} />{:else}{t('mail.noSubject')}{/if}</span>
        {#if r.snippet}<span class="snip faint">— <Vis text={r.snippet} /></span>{/if}
        {#if r.meta?.commit}<span class="chip neutral mono" title={r.meta.commit}>{r.meta.commit.slice(0, 8)}</span>{/if}
      </div>
      <div class="when faint">
        {#if r.delivery}
          {@const parts = deliverySummary(r.delivery)}
          <span class="dsum" data-testid="delivery-summary" title={t('mail.dsum.long')}>
            {#each parts as p, i}{#if i > 0}<span class="sep">·</span>{/if}<span class="ds ds-{p.state}" data-dstate={p.state}>{p.n} {t(`dlv.${p.state}`)}</span>{/each}
          </span>
        {/if}
        <span title={r.created_at}>{shortTime(r.created_at, ui.now, ui.lang)}</span>
      </div>
    </div>
  {/each}
</div>

<style>
  .list {
    display: flex;
    flex-direction: column;
  }
  .mrow {
    display: grid;
    grid-template-columns: minmax(90px, 160px) 1fr auto;
    gap: 10px;
    align-items: center;
    padding: 0 12px;
    height: 32px;
    border-bottom: 1px solid var(--border);
    cursor: pointer;
    background: var(--surface);
  }
  .mrow:hover {
    background: var(--surface-2);
  }
  .mrow.sel {
    background: var(--accent-weak);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .wb {
    font-family: var(--font);
    color: var(--muted);
  }
  .who {
    display: flex;
    align-items: center;
    gap: 5px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 12px;
  }
  .unread .who,
  .unread .s {
    font-weight: 650;
  }
  .udot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    flex: none;
  }
  .subj {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
  }
  .s {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .snip {
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .when {
    font-size: 12px;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .dsum {
    display: inline-flex;
    gap: 4px;
    font-size: 11.5px;
  }
  .ds {
    font-weight: 600;
    color: var(--text);
  }
  .ds-acked {
    color: var(--ok);
  }
  .ds-queued {
    color: var(--faint);
  }
  .src {
    font-family: var(--font);
    font-size: 10.5px;
    padding: 0 4px;
    border-radius: 3px;
    border: 1px solid currentColor;
    line-height: 14px;
  }
  .src-user {
    color: var(--src-user);
  }
  .src-system {
    color: var(--src-system);
    background: color-mix(in srgb, var(--src-system) 12%, transparent);
  }
  .src-external {
    color: var(--src-external);
  }
</style>
