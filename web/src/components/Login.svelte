<script lang="ts">
  import { ApiError } from '../lib/api';
  import { currentPage, pageUrl } from '../lib/base';
  import { store } from '../lib/store.svelte';
  const operator = currentPage() === 'admin';
  import { t, ui } from '../lib/ui.svelte';
  import LangTheme from './LangTheme.svelte';

  let token = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const value = token.trim();
    if (!value) return;
    busy = true;
    error = null;
    try {
      await store.login(value);
      token = '';
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) error = t('login.err.invalid');
      else if (err instanceof ApiError && err.status === 403) error = t('login.err.scope');
      else error = t('app.error', { msg: err instanceof Error ? err.message : String(err) });
    } finally {
      busy = false;
    }
  }
</script>

<main class="login" lang={ui.lang === 'zh' ? 'zh-CN' : 'en'}>
  <form class="card" onsubmit={submit}>
    <div class="row">
      <div class="logo" aria-hidden="true">✉</div>
      <h1>amail · {t('login.title')}</h1>
    </div>
    <p class="muted">{operator ? t('login.operator') : t('login.intro')}</p>
    <label for="token">{t('login.token')}</label>
    <input
      id="token"
      type="password"
      autocomplete="off"
      spellcheck="false"
      placeholder="amail_…"
      bind:value={token}
      disabled={busy}
    />
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    <div class="row">
      <span class="faint hint">{t('login.hint')}</span>
      <span class="spacer"></span>
      <button class="primary" type="submit" disabled={busy || !token.trim()}>
        {busy ? t('login.busy') : t('login.submit')}
      </button>
    </div>
    {#if store.info?.registration && !operator}
      <a class="reg" href={pageUrl('register')}>{t('login.register')}</a>
    {/if}
  </form>
  <div class="prefs"><LangTheme /></div>
</main>

<style>
  .login {
    min-height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 14px;
    padding: 24px 16px;
  }
  form {
    width: min(440px, 100%);
    padding: 22px 22px 18px;
    display: flex;
    flex-direction: column;
    gap: 10px;
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
  }
  input {
    font-family: var(--mono);
  }
  .hint {
    font-size: 12px;
  }
  .reg {
    font-size: 13px;
    align-self: flex-start;
  }
</style>
