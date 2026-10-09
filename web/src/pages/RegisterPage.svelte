<script lang="ts">
  import { ApiError, post } from '../lib/api';
  import { pageUrl } from '../lib/base';
  import { store } from '../lib/store.svelte';
  import { isKey } from '../lib/i18n';
  import { t, ui } from '../lib/ui.svelte';
  import type { RegisterResponse } from '../lib/types';
  import LangTheme from '../components/LangTheme.svelte';
  import DataNotice from '../components/DataNotice.svelte';

  let invite = $state(new URLSearchParams(globalThis.location?.search ?? '').get('invite') ?? '');
  let org = $state('');
  let userName = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let done = $state.raw<RegisterResponse | null>(null);
  let copied = $state(false);

  const origin = globalThis.location?.origin ?? '';
  const closed = $derived(store.info !== null && !store.info.registration);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      done = await post<RegisterResponse>('/v1/register', {
        invite: invite.trim(),
        org: org.trim(),
        user_name: userName.trim() || undefined,
      });
    } catch (err) {
      const key = err instanceof ApiError ? `reg.err.${err.code}` : '';
      error = isKey(key) ? t(key) : t('app.error', { msg: err instanceof Error ? err.message : String(err) });
    } finally {
      busy = false;
    }
  }

  async function copy() {
    await navigator.clipboard?.writeText(done?.token ?? '');
    copied = true;
  }

  async function signIn() {
    if (!done) return;
    await store.login(done.token);
    history.replaceState(null, '', `${pageUrl('')}#/org`);
  }
</script>

<main class="register" lang={ui.lang === 'zh' ? 'zh-CN' : 'en'}>
  <div class="card box">
    <div class="row">
      <div class="logo" aria-hidden="true">✉</div>
      <h1>amail · {t('reg.title')}</h1>
    </div>
    {#if done}
      <p data-testid="reg-done">{t('reg.done', { org: done.org })}</p>
      <code class="tok" data-testid="reg-token">{done.token}</code>
      <p class="warn">{t('reg.save')}</p>
      <div class="row">
        <button class="small" onclick={copy}>{copied ? '✓' : t('seat.copy')}</button>
        <span class="spacer"></span>
        <button class="primary" onclick={signIn}>{t('reg.signin')}</button>
      </div>
      <p class="muted">{t('reg.next')}</p>
      <ol class="steps">
        <li>{t('reg.step.login')}<pre class="cmds">amail login {origin}</pre></li>
        <li>{t('reg.step.node')}<pre class="cmds">amail node token --save
amail node start</pre></li>
        <li>{t('reg.step.init')}<pre class="cmds">amail init</pre></li>
        <li>{t('reg.step.edit')}</li>
        <li>{t('reg.step.up')}<pre class="cmds">amail up</pre></li>
      </ol>
    {:else if closed}
      <p class="error" role="alert">{t('reg.closed')}</p>
    {:else}
      <p class="muted">{t('reg.intro')}</p>
      <form onsubmit={submit}>
        <label for="r-invite">{t('reg.invite')}</label>
        <input id="r-invite" autocomplete="off" spellcheck="false" placeholder="inv_…" bind:value={invite} disabled={busy} />
        <label for="r-org">{t('reg.org')}</label>
        <input id="r-org" autocomplete="off" spellcheck="false" placeholder="acme" bind:value={org} disabled={busy} />
        <span class="faint hint">{t('reg.orgHint')}</span>
        <label for="r-user">{t('reg.userName')}</label>
        <input id="r-user" autocomplete="off" spellcheck="false" placeholder="owner" bind:value={userName} disabled={busy} />
        <span class="faint hint">{t('reg.userNameHint')}</span>
        {#if error}<p class="error" role="alert">{error}</p>{/if}
        <div class="row">
          <a href={pageUrl('')}>{t('reg.back')}</a>
          <span class="spacer"></span>
          <button class="primary" type="submit" disabled={busy || !invite.trim() || !org.trim()}>
            {busy ? t('reg.busy') : t('reg.submit')}
          </button>
        </div>
      </form>
    {/if}
    <DataNotice compact />
  </div>
  <div class="prefs"><LangTheme /></div>
</main>

<style>
  .register {
    min-height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 14px;
    padding: 24px 16px;
  }
  .box {
    width: min(560px, 100%);
    padding: 22px 22px 18px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  form {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .logo {
    width: 28px;
    height: 28px;
    border-radius: 7px;
    background: var(--accent);
    color: var(--accent-text);
    display: grid;
    place-items: center;
    font-size: 16px;
  }
  p {
    margin: 0;
  }
  label {
    font-weight: 600;
    margin-top: 4px;
  }
  input {
    font-family: var(--mono);
  }
  .hint {
    font-size: 12px;
  }
  .tok {
    display: block;
    user-select: all;
    word-break: break-all;
    padding: 8px 10px;
    background: var(--warn-weak);
    border: 1px solid var(--warn);
    border-radius: 4px;
  }
  .warn {
    font-size: 13px;
  }
  .steps {
    margin: 0;
    padding-left: 20px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .steps .cmds {
    margin-top: 3px;
  }
  .cmds {
    margin: 0;
    padding: 8px 10px;
    font-size: 12px;
    background: var(--surface-2);
    border-radius: 4px;
    overflow-x: auto;
  }
</style>
