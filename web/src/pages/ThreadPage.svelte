<script lang="ts">
  import Vis from '../components/Vis.svelte';
  import { onDestroy, tick } from 'svelte';
  import { get, del, enc } from '../lib/api';
  import { t, ui } from '../lib/ui.svelte';
  import { store } from '../lib/store.svelte';
  import { threadFolder } from '../lib/nav';
  import { go, href } from '../lib/router.svelte';
  import type { DeleteMailResponse, ThreadResponse } from '../lib/types';
  import MessageCard from '../components/MessageCard.svelte';

  let { id, focus }: { id: number; focus?: number } = $props();
  let data = $state.raw<ThreadResponse | null>(null);
  let error = $state<string | null>(null);
  let note = $state<string | null>(null);
  let reload = $state(0);
  let deleting = $state(false);
  /** Every message of the thread was deleted: there is nothing to load. */
  let gone = $state(false);

  $effect(() => {
    const tid = id;
    void reload;
    data = null;
    error = null;
    gone = false;
    get<ThreadResponse>(`/v1/threads/${enc(String(tid))}`)
      .then(async (r) => {
        data = r;
        ui.thread = { id: tid, folder: threadFolder(r.messages, store.me?.seat, focus) };
        await tick();
        if (focus) document.querySelector(`article[data-msg="${focus}"]`)?.scrollIntoView({ block: 'center' });
      })
      .catch((e) => (error = e instanceof Error ? e.message : String(e)));
  });

  // Web sessions are user sessions: the user may delete what the org may
  // delete of the thread (the server decides which messages).
  async function remove() {
    if (deleting || !confirm(t('thread.confirmDelete'))) return;
    deleting = true;
    try {
      let deleted = 0;
      let r: DeleteMailResponse;
      do {
        r = await del<DeleteMailResponse>(`/v1/threads/${enc(String(id))}`);
        deleted += r.deleted;
      } while (r.more);
      note = r.kept > 0 ? t('thread.deletedKept', { deleted, kept: r.kept }) : t('thread.deleted', { deleted });
      // Messages that stay are shown again; with none left the thread is
      // gone and is not asked for again.
      if (r.kept > 0) reload++;
      else {
        data = null;
        gone = true;
      }
    } catch (e) {
      note = e instanceof Error ? e.message : String(e);
    } finally {
      deleting = false;
    }
  }

  onDestroy(() => {
    if (ui.thread?.id === id) ui.thread = null;
  });

  const participants = $derived(data ? [...new Set(data.messages.map((m) => m.from))] : []);
</script>

<div class="page thread">
  {#if note}<p class="faint">{note}</p>{/if}
  {#if gone}
    <p><a href={href(['archive'])}>{t('thread.toArchive')}</a></p>
  {:else if error}
    <p class="error">{t('thread.notFound')} ({error})</p>
  {:else if !data}
    <p class="faint">{t('app.loading')}</p>
  {:else}
    <div class="page-head">
      <h1>{#if data.messages[0]?.subject}<Vis text={data.messages[0].subject} />{:else}{t('mail.noSubject')}{/if}</h1>
      <span class="faint">{t('thread.title', { id: data.thread_id })} · {t('thread.messages', { n: data.messages.length })}</span>
      <span class="spacer"></span>
      <span class="parts">{#each participants as p}<a class="mono chip neutral" href={p.includes('@') ? undefined : href(['org', p])}>{p}</a>{/each}</span>
      <button class="small danger" disabled={deleting} onclick={remove}>{t('thread.delete')}</button>
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
