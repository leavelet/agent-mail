// Interface preferences: language and theme, plus a clock for relative times.

import { langFor, translate, type Key, type Lang } from './i18n';
import { load, save } from './storage';

export type Theme = 'system' | 'light' | 'dark';

function initialLang(): Lang {
  const saved = load('amail.lang');
  if (saved === 'en' || saved === 'zh') return saved;
  return langFor(globalThis.navigator?.language);
}

function initialTheme(): Theme {
  const saved = load('amail.theme');
  return saved === 'light' || saved === 'dark' ? saved : 'system';
}

class Ui {
  lang = $state<Lang>(initialLang());
  theme = $state<Theme>(initialTheme());
  /** Ticks every few seconds so durations and presence staleness update. */
  now = $state(Date.now());
  helpOpen = $state(false);
  /** At phone width the navigation is a menu behind a button in the top
   * bar; this says whether it is open. */
  navOpen = $state(false);
  /** The thread page in view and the folder its messages suggest (for the
   * sidebar when the thread was opened by its URL). */
  thread = $state<{ id: number; folder: 'inbox' | 'sent' | null } | null>(null);

  setLang(l: Lang) {
    this.lang = l;
    save('amail.lang', l);
    applyLang(l);
  }

  setTheme(t: Theme) {
    this.theme = t;
    save('amail.theme', t === 'system' ? null : t);
    applyTheme(t);
  }
}

export const ui = new Ui();

export function t(key: Key, params?: Record<string, string | number>): string {
  return translate(ui.lang, key, params);
}

export function applyTheme(theme: Theme): void {
  const root = globalThis.document?.documentElement;
  if (!root) return;
  if (theme === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', theme);
}

export function applyLang(lang: Lang): void {
  const root = globalThis.document?.documentElement;
  if (root) root.lang = lang === 'zh' ? 'zh-CN' : 'en';
}

export function startClock(): () => void {
  const id = setInterval(() => (ui.now = Date.now()), 5000);
  return () => clearInterval(id);
}
