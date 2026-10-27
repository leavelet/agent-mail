<script lang="ts">
  // Create a document (`POST /v1/docs`) as the user seat. The kind's
  // required sections are put into the body as headings; while the title
  // is typed, documents with a similar title are shown (nothing blocks the
  // create).
  import { untrack } from 'svelte';
  import Vis from '../components/Vis.svelte';
  import { get, post } from '../lib/api';
  import { store } from '../lib/store.svelte';
  import { t } from '../lib/ui.svelte';
  import { go, href } from '../lib/router.svelte';
  import { bodyTemplate, docHref, docId, docStatusTone, parseList } from '../lib/docs';
  import { docError, lastCreated, titleCandidates } from '../lib/docops';
  import type { DocDetail, DocKind, DocRef } from '../lib/types';

  let { query }: { query: URLSearchParams } = $props();

  let kinds = $state.raw<DocKind[]>([]);
  let kind = $state('');
  let title = $state('');
  let description = $state('');
  let slug = $state('');
  let team = $state('');
  let labels = $state('');
  let body = $state('');
  let error = $state<string | null>(null);
  let busy = $state(false);
  let candidates = $state.raw<DocRef[]>([]);
  let looking = $state(false);
  /** The template last put into the body: replaced when the kind changes
   * while the body is still the template. */
  let template = '';

  $effect(() => {
    team = query.get('team') ?? '';
    const k = query.get('kind');
    if (k) kind = k;
  });

  $effect(() => {
    get<{ kinds: DocKind[] }>('/v1/docs/kinds')
      .then((r) => {
        kinds = r.kinds.filter((k) => !k.disabled);
        if (!kind) kind = kinds.find((k) => k.name === 'reference')?.name ?? kinds[0]?.name ?? '';
      })
      .catch((e) => (error = docError(e)));
  });

  const kindInfo = $derived(kinds.find((k) => k.name === kind));

  // The required sections as headings, while the body is empty or still
  // the template of another kind.
  $effect(() => {
    const info = kindInfo;
    if (!info) return;
    untrack(() => {
      if (body.trim() === '' || body === template) {
        template = bodyTemplate(info.required_sections);
        body = template;
      }
    });
  });

  // Similar documents while the title is typed.
  let timer: ReturnType<typeof setTimeout> | undefined;
  let gen = 0;
  $effect(() => {
    const words = title.trim();
    const k = kind;
    clearTimeout(timer);
    if ([...words].length < 3) {
      candidates = [];
      return;
    }
    timer = setTimeout(async () => {
      const g = ++gen;
      looking = true;
      try {
        const r = await titleCandidates(words, k);
        if (g === gen) candidates = r;
      } catch {
        if (g === gen) candidates = [];
      } finally {
        if (g === gen) looking = false;
      }
    }, 300);
    return () => clearTimeout(timer);
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    error = null;
    busy = true;
    try {
      const d = await post<DocDetail>('/v1/docs', {
        title: title.trim(),
        kind: kind || undefined,
        description: description.trim() || undefined,
        slug: slug.trim() || undefined,
        team: team || undefined,
        labels: parseList(labels),
        body: body.trim() ? body : undefined,
      });
      lastCreated.detail = d;
      go(['docs', docId(d.doc.num)]);
    } catch (err) {
      error = docError(err);
    } finally {
      busy = false;
    }
  }
</script>

<div class="page newdoc">
  <p class="back"><a href={href(['docs'])}>← {t('docs.title')}</a></p>
  <div class="page-head"><h1>{t('docs.new')}</h1></div>
  <form class="card" onsubmit={submit} data-testid="doc-new-form">
    <div class="row wrap">
      <label>{t('doc.field.kind')}
        <select bind:value={kind} data-testid="doc-new-kind">
          {#each kinds as k (k.name)}<option value={k.name}>{k.name}</option>{/each}
        </select>
      </label>
      <label>{t('doc.field.team')}
        <select bind:value={team}>
          <option value="">{t('doc.ownerTeam')}</option>
          {#each store.teams as tm}<option value={tm.name}>{tm.name}</option>{/each}
        </select>
      </label>
      <label>{t('doc.field.slug')}<input class="mono" maxlength="128" placeholder="runbook.deploy" bind:value={slug} /></label>
    </div>
    {#if kindInfo}
      <p class="faint hint">{kindInfo.description}{#if kindInfo.required_sections.length} · {t('doc.requiredSections', { list: kindInfo.required_sections.join(', ') })}{/if}{#if kindInfo.activate_by === 'human'} · {t('doc.humanActivates')}{/if}</p>
    {/if}
    <label>{t('doc.field.title')}<input id="doc-title" required maxlength="512" bind:value={title} autocomplete="off" /></label>
    {#if candidates.length}
      <div class="cands" role="status" data-testid="doc-candidates">
        <span>{t('doc.candidates')}</span>
        <ul>
          {#each candidates as c (c.num)}
            <li>
              <a class="mono" href={docHref(c.num)} target="_blank" rel="noopener">{docId(c.num)}</a>
              <a href={docHref(c.num)} target="_blank" rel="noopener"><Vis text={c.title} /></a>
              <span class="chip neutral">{c.kind}</span>
              <span class="chip tone-{docStatusTone(c.status)}">{t(`doc.status.${c.status}`)}</span>
              {#if c.description}<span class="faint"><Vis text={c.description} /></span>{/if}
            </li>
          {/each}
        </ul>
        <span class="faint">{t('doc.candidatesHint')}</span>
      </div>
    {:else if looking}<span class="faint hint">{t('doc.lookingForSimilar')}</span>{/if}
    <label>{t('doc.field.description')}<input maxlength="200" placeholder={t('doc.descriptionHint')} bind:value={description} /></label>
    <label>{t('doc.field.labels')}<input placeholder={t('task.labelsHint')} bind:value={labels} /></label>
    <label>{t('doc.body')}<textarea class="mono" rows="16" bind:value={body} data-testid="doc-new-body"></textarea></label>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="row">
      <button class="primary" type="submit" disabled={busy || !title.trim()}>{t('doc.create')}</button>
      <span class="faint hint">{t('doc.createHint')}</span>
    </div>
  </form>
</div>

<style>
  .newdoc {
    max-width: 860px;
  }
  .back {
    margin: 0 0 6px;
    font-size: 12px;
  }
  form {
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
    color: var(--muted);
  }
  textarea {
    width: 100%;
    font-size: 12px;
  }
  .hint {
    font-size: 12px;
    margin: 0;
  }
  .cands {
    padding: 6px 10px;
    border-radius: var(--radius);
    background: var(--notice-weak);
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }
  .cands ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .cands li {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  @media (max-width: 720px) {
    .newdoc {
      padding: 12px 16px;
    }
  }
</style>
