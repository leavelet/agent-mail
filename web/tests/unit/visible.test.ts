import { describe, expect, it } from 'vitest';
import { isUnsafe, segments, visible } from '../../src/lib/visible';

describe('visible', () => {
  it('keeps ordinary text', () => {
    const s = '数据库 schema ✓ é\tx\ny';
    expect(visible(s)).toBe(s);
    expect(segments(s)).toEqual([{ text: s, escaped: false }]);
  });
  it('escapes bidi, zero-width, tag and control characters', () => {
    expect(visible('ab‮cd')).toBe('ab\\u{202e}cd');
    expect(visible('​﻿⁦')).toBe('\\u{200b}\\u{feff}\\u{2066}');
    expect(visible('x\u{e0041}')).toBe('x\\u{e0041}');
    expect(visible('\u001b]0;t\u0007')).toBe('\\u{1b}]0;t\\u{7}');
    expect(visible('a\u009bb\u007f')).toBe('a\\u{9b}b\\u{7f}');
    expect(visible('a\rb c')).toBe('a\\u{d}b\\u{2028}c');
  });
  it('escapes default-ignorable code points and the braille blank', () => {
    for (const cp of [0xfe00, 0xfe0f, 0xe0100, 0xe0101, 0xe01ef, 0x034f, 0x17b4, 0x17b5, 0x180b, 0x180f, 0x2065, 0x2800, 0xfff0, 0xe0000, 0xe0fff]) {
      const c = String.fromCodePoint(cp);
      expect(isUnsafe(c), cp.toString(16)).toBe(true);
      expect(visible(`a${c}b`)).toBe(`a\\u{${cp.toString(16)}}b`);
    }
    for (const cp of [0x17b3, 0x17b6, 0x2801, 0xfe10, 0xe1000]) {
      expect(isUnsafe(String.fromCodePoint(cp)), cp.toString(16)).toBe(false);
    }
  });
  it('splits text into plain parts and escapes', () => {
    expect(segments('ok️\u{e0101}!‮')).toEqual([
      { text: 'ok', escaped: false },
      { text: '\\u{fe0f}\\u{e0101}', escaped: true },
      { text: '!', escaped: false },
      { text: '\\u{202e}', escaped: true },
    ]);
  });
});
