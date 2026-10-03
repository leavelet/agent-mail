import { describe, expect, it } from 'vitest';
import { dictionaries, langFor, translate } from '../../src/lib/i18n';

describe('i18n', () => {
  it('has the same keys and placeholders in English and Chinese', () => {
    const en = dictionaries.en;
    const zh = dictionaries.zh;
    expect(Object.keys(zh).sort()).toEqual(Object.keys(en).sort());
    const ph = (s: string) => (s.match(/\{[a-z]+\}/g) ?? []).sort();
    for (const k of Object.keys(en) as (keyof typeof en)[]) {
      expect(zh[k].length, k).toBeGreaterThan(0);
      expect(ph(zh[k]), k).toEqual(ph(en[k]));
    }
  });
  it('fills placeholders and picks the language', () => {
    expect(translate('en', 'org.showing', { shown: 3, total: 10 })).toBe('3 of 10 seats');
    expect(translate('zh', 'org.showing', { shown: 3, total: 10 })).toBe('显示 3 / 10 个席位');
    expect(langFor('zh-CN')).toBe('zh');
    expect(langFor('en-US')).toBe('en');
    expect(langFor(undefined)).toBe('en');
  });
});
