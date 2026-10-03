<script lang="ts">
  import { tick } from 'svelte';
  import { get, enc } from '../lib/api';
  import { t } from '../lib/ui.svelte';
  import { go, href } from '../lib/router.svelte';
  import type { ThreadResponse } from '../lib/types';
  import MessageCard from '../components/MessageCard.svelte';

  let { id, focus }: { id: number; focus?: number } = $props();
  let data = $state.raw<ThreadResponse | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    const tid = id;
    data = null;
    error = null;
    get<ThreadResponse>(`/v1/threads/${enc(String(tid))}`)
      .then(async (r) => {
        data = r;
        await tick();
        if (focus) document.querySelector(`article[data-msg="${focus}"]`)?.scrollIntoView({ block: 'center' });
      })
      .catch((e) => (error = e instanceof Error ? e.message : String(e)));
  });

  const participants = $derived(data ? [...new Set(data.messages.map((m) => m.from))] : []);
</script>

<div class="page thread">
  {#if error}
    <p class="error">{t('thread.notFound')} ({error})</p>
  {:else if !data}
    <p class="faint">{t('app.loading')}</p>
  {:else}
    <div class="page-head">
      <h1>{data.messages[0]?.subject || t('mail.noSubject')}</h1>
      <span class="faint">{t('thread.title', { id: data.thread_id })} · {t('thread.messages', { n: data.messages.length })}</span>
      <span class="spacer"></span>
      <span class="parts">{#each participants as p}<a class="mono chip neutral" href={p.includes('@') ? undefined : href(['org', p])}>{p}</a>{/each}</span>
    </div>
    <div class="msgs">
      {#each data.messages as m (m.id)}
        <div>
          <MessageCard message={m} focus={m.id === focus}>
            {#snippet actions()}
              <button class="small" onclick={() => go(['compose'], { reply: m.id })}>{t('mail.reply')}</button>
              {#if m.to.length + m.cc.length > 1}
                <button class="small" onclick={() => go(['compose'], { reply: m.id, all: 1 })}>{t('mail.replyAll')}</button>
              {/if}
            {/snippet}
          </MessageCard>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .thread {
    max-width: 980px;
  }
  .msgs {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .parts {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
  }
</style>
