<script lang="ts">
  import Vis from './Vis.svelte';
  import { store } from '../lib/store.svelte';
  // Lists of the org, to say through which one mail arrived.
  const isList = (a: string) => store.lists.some((l) => l.name === a);
  import { t, ui } from '../lib/ui.svelte';
  import { href, go, router } from '../lib/router.svelte';
  import { get, patch, qs, enc, ApiError } from '../lib/api';
  import { grantsOf } from '../lib/openings';
  import OpeningsEditor from './OpeningsEditor.svelte';
  import { KNOWN_ATTENTION, focusUntil, seatStatus, stateLongKey, nodeOpBlock, terminalHref } from '../lib/presence';
  import { duration, shortTime } from '../lib/time';
  import { fromEntry, fromSent, newestFirst, type MailRow } from '../lib/mailrows';
  import type { ArchiveResponse, InboxResponse } from '../lib/types';
  import StateBadge from './StateBadge.svelte';
  import Transcript from './Transcript.svelte';
  import MailList from './MailList.svelte';
  import SeatTasks from './SeatTasks.svelte';
  import SeatHandover from './SeatHandover.svelte';
  import SeatWorklog from './SeatWorklog.svelte';

  let { name }: { name: string } = $props();

  type Tab = 'mirror' | 'inbox' | 'sent' | 'handover' | 'worklog';
  const TABS: Tab[] = ['mirror', 'inbox', 'sent', 'handover', 'worklog'];
  // `?tab=handover` in the URL opens that tab.
  const asked = $derived(router.route.query.get('tab') as Tab | null);
  let tab = $state<Tab>('mirror');
  $effect(() => {
    if (asked && TABS.includes(asked)) tab = asked;
  });
  let rows = $state.raw<MailRow[]>([]);
  let mailError = $state<string | null>(null);
  let opMsg = $state<string | null>(null);
  let opBusy = $state(false);
  let copied = $state(false);

  const seat = $derived(store.seatMap.get(name));
  const now = $derived(ui.now);
  // The same status as the seat's row on the org page.
  const status = $derived(seat ? seatStatus(seat, now) : undefined);
  const p = $derived(status?.presence);
  const seatState = $derived(status?.state ?? 'offline');
  const age = $derived(status?.age);
  const focus = $derived(seat ? focusUntil(seat, now) : undefined);
  const children = $derived(store.seats.filter((s) => s.parent === name).map((s) => s.name));
  const block = $derived(seat ? nodeOpBlock(seat, now) : 'noPresence');
  const seq = $derived(store.counts.get(name)?.seq ?? 0);
  const attachCmd = $derived(`amail attach ${name}`);

  // Reload the mail tab when the seat's mailbox changes.
  $effect(() => {
    void seq;
    if (tab !== 'inbox' && tab !== 'sent') return;
    const which = tab;
    mailError = null;
    const load =
      which === 'inbox'
        ? get<InboxResponse>(`/v1/inbox${qs({ seat: name, before_seq: Number.MAX_SAFE_INTEGER, limit: 100 })}`).then((r) =>
            newestFirst(r.entries.map((e) => fromEntry(e, isList))),
          )
        : get<ArchiveResponse>(`/v1/sent${qs({ seat: name, limit: 100, delivery: 'true' })}`).then(fromSent);
    load
      .then((r) => {
        if (tab === which) rows = r;
      })
      .catch((e) => (mailError = e instanceof Error ? e.message : String(e)));
  });

  async function op(kind: 'sleep' | 'wake') {
    opBusy = true;
    opMsg = null;
    try {
      const r = await store.seatOp(name, kind);
      opMsg = t('seat.opSent', { op: t(kind === 'sleep' ? 'seat.sleep' : 'seat.wake'), n: r.nodes });
    } catch (e) {
      opMsg = e instanceof ApiError && e.code === 'node_offline' ? t('seat.disabled.noNode') : e instanceof Error ? e.message : String(e);
    } finally {
      opBusy = false;
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(attachCmd);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      // Clipboard unavailable: the command is selectable text.
    }
  }

  const blockReason = $derived(block ? t(`seat.disabled.${block}`) : '');
  const tag = $derived(status?.tag);
  const attnText = $derived.by(() => {
    if (!tag) return null;
    return KNOWN_ATTENTION.includes(tag) ? t(`attn.${tag}` as 'attn.login') : tag;
  });
  const starting = $derived(!!status?.starting);
  const tf = $derived(p?.turn_failed);
  const terminal = $derived(terminalHref(p?.terminal_url));

  // The description: what others read about the seat (other orgs too, when open).
  let editingDesc = $state(false);
  let desc = $state('');
  let descError = $state<string | null>(null);
  function editDesc() {
    desc = seat?.description ?? '';
    descError = null;
    editingDesc = true;
  }
  async function saveDesc(e: SubmitEvent) {
    e.preventDefault();
    descError = null;
    try {
      await patch(`/v1/seats/${enc(name)}`, { description: desc.trim() || null });
      await store.refreshSeats();
      editingDesc = false;
    } catch (err) {
      descError = err instanceof Error ? err.message : String(err);
    }
  }
  // Audiences the seat is open to only as a contact of its team.
  const inherited = $derived.by(() => {
    const own = new Set(grantsOf(store.grants, name).map((g) => g.audience));
    return (seat?.open_to ?? []).filter((a) => !own.has(a));
  });
</script>

{#if !seat}
  <div class="empty">{store.seats.length ? t('seat.notFound', { name }) : t('app.loading')}</div>
{:else}
  <div class="detail">
    <div class="head">
      <div class="title">
        <h1 class="mono">{seat.name}</h1>
        {#if seat.display_name}<span class="muted"><Vis text={seat.display_name} /></span>{/if}
        {#if seat.kind === 'agent'}<StateBadge state={seatState} {age} {starting} failing={!!tf} long />{:else}<span class="chip neutral">{t('org.you')}</span>{/if}
        {#if focus}<span class="chip focus" title={t('flag.focus.long')}>◐ {t('flag.focus', { time: shortTime(focus, now, ui.lang) })}</span>{/if}
      </div>
      <div class="actions row wrap">
        {#if seat.kind === 'agent'}
          <a class="btnlike primary" href={href(['compose'], { to: seat.name })}>{t('seat.write')}</a>
          <button disabled={!!block || opBusy} title={blockReason} onclick={() => op('sleep')}>{t('seat.sleep')}</button>
          <button disabled={!!block || opBusy} title={blockReason} onclick={() => op('wake')}>{t('seat.wake')}</button>
        {/if}
        <button class="ghost small close" aria-label="close" onclick={() => go(['org'])}>✕</button>
      </div>
    </div>
    {#if block && seat.kind === 'agent'}<p class="faint reason">{blockReason}</p>{/if}
    {#if opMsg}<p class="opmsg" role="status">{opMsg}</p>{/if}

    {#if p?.attention && p.attention !== 'turn-failed'}
      <div class="alert" role="alert" data-testid="seat-attention">
        <strong>⚠ {t('seat.attention')}: {attnText}</strong>
        {#if p.attention === 'trust-prompt'}
          <span data-testid="trust-help">{terminal ? t('seat.trust.web') : t('seat.trust.cli', { cmd: attachCmd })}</span>
        {:else if p.attention_hint}<span><Vis text={p.attention_hint} /></span>{/if}
      </div>
    {:else if p?.wake_failed && !tf}
      <div class="alert" role="alert">
        <strong>⚠ {t('seat.attention')}: {t('attn.wake-failed')}</strong>
        <span>{t('seat.wakeFailed')}</span>
      </div>
    {/if}
    {#if tf}
      <div class="alert" role="alert" data-testid="turn-failed">
        <strong>⚠ {t('seat.turnFailed.title')}</strong>
        <span>{t('seat.turnFailed.reason')} <code class="reason-text"><Vis text={tf.reason} /></code></span>
        <span>{t('seat.turnFailed.count', { n: tf.count, time: shortTime(tf.at, now, ui.lang) })}</span>
        <span>{tf.retrying ? t('seat.turnFailed.retrying') : t('seat.turnFailed.stopped')}</span>
        <span>{t('seat.turnFailed.todo')}</span>
      </div>
    {/if}

    <dl class="facts">
      <dt>{t('seat.description')}</dt>
      <dd data-testid="seat-description">
        {#if editingDesc}
          <form class="descform" onsubmit={saveDesc}>
            <textarea rows="2" maxlength="500" aria-label={t('seat.description')} bind:value={desc}></textarea>
            <div class="row">
              <button class="small primary" type="submit">{t('admin.save')}</button>
              <button class="small" type="button" onclick={() => (editingDesc = false)}>{t('admin.cancel')}</button>
              <span class="faint">{t('seat.descriptionHint')}</span>
            </div>
            {#if descError}<p class="error" role="alert">{descError}</p>{/if}
          </form>
        {:else}
          {#if seat.description}<span class="desc"><Vis text={seat.description} /></span>{:else}<span class="faint">{t('seat.noDescription')}</span>{/if}
          <button class="small ghost" onclick={editDesc}>{t('admin.edit')}</button>
        {/if}
      </dd>
      <dt>{t('seat.team')}</dt>
      <dd>
        {#if seat.group}<a class="mono" href={href(['teams', seat.group])}>{seat.group}</a>{#if seat.contact}<span class="chip neutral" title={t('team.contact.long')}>{t('team.contact')}</span>{/if}{:else}<span class="faint">{t('seat.noTeam')}</span>{/if}
      </dd>
      <dt>{t('open.title')}</dt>
      <dd><OpeningsEditor target={seat.name} team={seat.contact ? seat.group : undefined} {inherited} /></dd>
      <dt>{t('seat.parent')}</dt>
      <dd>{#if seat.parent}<a class="mono" href={href(['org', seat.parent])}>{seat.parent}</a>{:else}<span class="faint">{t('seat.none')}</span>{/if}</dd>
      <dt>{t('seat.children')}</dt>
      <dd class="kids">
        {#if children.length === 0}<span class="faint">{t('seat.none')}</span>{/if}
        {#each children.slice(0, 40) as c}<a class="mono" href={href(['org', c])}>{c}</a>{/each}
        {#if children.length > 40}<span class="faint">+{children.length - 40}</span>{/if}
      </dd>
      {#if seat.kind === 'agent'}
        <dt>{t('seat.state')}</dt>
        <dd data-testid="seat-state">{t(stateLongKey({ state: seatState, starting }))}{#if age !== undefined} · {t('seat.for', { d: duration(age, ui.lang) })}{/if}</dd>
        <dt>{t('seat.harness')}</dt>
        <dd><Vis text={[seat.harness ?? p?.harness, seat.model].filter(Boolean).join(' · ') || '—'} />{#if seat.permission_mode} · <span class="faint">{seat.permission_mode}</span>{/if}</dd>
        <dt>{t('seat.node')}</dt>
        <dd>{#if p?.node}<span class="mono">{p.node}</span>{:else if p}<span class="faint">—</span>{:else}<span class="faint">{t('seat.noNode')}</span>{/if}</dd>
        <dt>{t('seat.terminal')}</dt>
        <dd>
          {#if terminal}
            <a href={terminal} target="_blank" rel="noopener noreferrer">{t('seat.openTerminal')} ↗</a>
          {:else}
            <span class="faint">{t('seat.attachCmd')}</span>
            <code class="cmd">{attachCmd}</code>
            <button class="small ghost" onclick={copy}>{copied ? t('seat.copied') : t('seat.copy')}</button>
          {/if}
        </dd>
      {/if}
    </dl>
    <details class="role" open={!!seat.role && seat.role.length < 400}>
      <summary>{t('seat.role')}</summary>
      {#if seat.role}<pre><Vis text={seat.role} /></pre>{:else}<p class="faint">{t('seat.noRole')}</p>{/if}
    </details>

    <SeatTasks seat={seat.name} />

    <div class="tabs" role="tablist">
      {#if seat.kind === 'agent'}
        <button role="tab" aria-selected={tab === 'mirror'} onclick={() => (tab = 'mirror')}>{t('seat.mirror')}</button>
      {/if}
      <button role="tab" aria-selected={tab === 'inbox'} onclick={() => (tab = 'inbox')}>
        {t('seat.inbox')}{#if store.counts.get(name)?.unread}<span class="count hot">{store.counts.get(name)?.unread}</span>{/if}
      </button>
      <button role="tab" aria-selected={tab === 'sent'} onclick={() => (tab = 'sent')}>{t('seat.sent')}</button>
      {#if seat.kind === 'agent'}
        <button role="tab" aria-selected={tab === 'handover'} onclick={() => (tab = 'handover')} data-testid="tab-handover">{t('seat.handover')}</button>
        <button role="tab" aria-selected={tab === 'worklog'} onclick={() => (tab = 'worklog')} data-testid="tab-worklog">{t('seat.worklog')}</button>
      {/if}
    </div>
    <div class="tabbody">
      {#if tab === 'mirror' && seat.kind === 'agent'}
        <Transcript seat={seat.name} />
      {:else if tab === 'mirror'}
        <div class="empty">—</div>
      {:else if tab === 'handover' && seat.kind === 'agent'}
        <SeatHandover seat={seat.name} />
      {:else if tab === 'worklog' && seat.kind === 'agent'}
        <SeatWorklog seat={seat.name} />
      {:else if mailError}
        <p class="error">{mailError}</p>
      {:else if rows.length === 0}
        <div class="empty">{t('mail.empty')}</div>
      {:else}
        <div class="mail card">
          <MailList
            {rows}
            showTo={tab === 'sent'}
            label={tab === 'sent' ? t('seat.sent') : t('seat.inbox')}
            onselect={(r) => go(['thread', r.thread_id], { m: r.id })}
          />
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .detail {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 16px;
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: 1;
    min-width: 0;
  }
  h1 {
    font-size: 17px;
  }
  .btnlike {
    display: inline-block;
    border-radius: var(--radius);
    padding: 3px 10px;
    line-height: 20px;
    border: 1px solid var(--accent);
  }
  .btnlike.primary {
    background: var(--accent);
    color: var(--accent-text);
    font-weight: 600;
  }
  .btnlike:hover {
    text-decoration: none;
    filter: brightness(1.08);
  }
  .reason,
  .opmsg {
    margin: 0;
    font-size: 12px;
  }
  .alert {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 7px 10px;
    border-radius: var(--radius);
    background: var(--danger-weak);
    color: var(--danger);
  }
  .alert span {
    color: var(--text);
  }
  .reason-text {
    white-space: pre-wrap;
    word-break: break-word;
  }
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 3px 14px;
    margin: 0;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    min-width: 0;
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: center;
  }
  .kids {
    gap: 4px 10px;
  }
  .desc {
    white-space: pre-wrap;
    word-break: break-word;
  }
  .descform {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
    min-width: 0;
  }
  .descform textarea {
    width: 100%;
  }
  .cmd {
    padding: 1px 6px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 4px;
    user-select: all;
  }
  .role summary {
    cursor: pointer;
    color: var(--muted);
  }
  .role pre {
    margin-top: 4px;
    padding: 8px 10px;
    background: var(--surface-2);
    border-radius: var(--radius);
    font-family: var(--font);
    max-height: 180px;
    overflow: auto;
  }
  .role p {
    margin: 4px 0;
  }
  .tabs .count {
    margin-left: 5px;
  }
  .tabbody {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .mail {
    overflow: auto;
    min-height: 0;
  }
</style>
