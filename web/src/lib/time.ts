// Compact durations and times.

import type { Lang } from './i18n';

/** A short duration: 45s, 12m, 3h 5m, 4d 2h. */
export function duration(ms: number, lang: Lang = 'en'): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const zh = lang === 'zh';
  const u = zh ? { s: '秒', m: '分', h: '小时', d: '天' } : { s: 's', m: 'm', h: 'h', d: 'd' };
  const sp = zh ? '' : ' ';
  if (s < 60) return `${s}${u.s}`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}${u.m}`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}${u.h}${sp}${m % 60}${u.m}` : `${h}${u.h}`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d}${u.d}${sp}${h % 24}${u.h}` : `${d}${u.d}`;
}

/** Time of day for today, else a short date and time. */
export function shortTime(iso: string | undefined, now: number, lang: Lang = 'en'): string {
  if (!iso) return '';
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return '';
  const locale = lang === 'zh' ? 'zh-CN' : 'en-GB';
  const n = new Date(now);
  const sameDay =
    t.getFullYear() === n.getFullYear() && t.getMonth() === n.getMonth() && t.getDate() === n.getDate();
  if (sameDay) return t.toLocaleTimeString(locale, { hour: '2-digit', minute: '2-digit' });
  const sameYear = t.getFullYear() === n.getFullYear();
  return t.toLocaleString(locale, {
    year: sameYear ? undefined : 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

export function fullTime(iso: string | undefined, lang: Lang = 'en'): string {
  if (!iso) return '';
  const t = new Date(iso);
  if (Number.isNaN(t.getTime())) return iso;
  return t.toLocaleString(lang === 'zh' ? 'zh-CN' : 'en-GB', {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}
