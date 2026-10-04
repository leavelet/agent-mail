// Showing characters that a reader cannot see.
//
// Mail content (bodies, subjects, `meta` values, display names, role text,
// list descriptions, token labels) is written by someone else. It can carry
// characters that render as nothing or change how the text around them is
// shown: bidirectional controls, zero-width characters, variation selectors,
// tag characters, control characters. The web UI shows each of them as a
// visible escape, `\u{202e}`, the same form the CLI and the text given to
// agents use (`amail_proto::text`).

// Every code point with the Unicode property Default_Ignorable_Code_Point,
// every format character (category Cf), and U+2800 BRAILLE PATTERN BLANK.
// Generated from Unicode 17.0.0 by `scripts/invisible-chars.py`; keep it
// equal to `is_invisible` in crates/amail-proto/src/text.rs.
const INVISIBLE: [number, number][] = [
  [0x00ad, 0x00ad],
  [0x034f, 0x034f],
  [0x0600, 0x0605],
  [0x061c, 0x061c],
  [0x06dd, 0x06dd],
  [0x070f, 0x070f],
  [0x0890, 0x0891],
  [0x08e2, 0x08e2],
  [0x115f, 0x1160],
  [0x17b4, 0x17b5],
  [0x180b, 0x180f],
  [0x200b, 0x200f],
  [0x202a, 0x202e],
  [0x2060, 0x206f],
  [0x2800, 0x2800],
  [0x3164, 0x3164],
  [0xfe00, 0xfe0f],
  [0xfeff, 0xfeff],
  [0xffa0, 0xffa0],
  [0xfff0, 0xfffb],
  [0x110bd, 0x110bd],
  [0x110cd, 0x110cd],
  [0x13430, 0x1343f],
  [0x1bca0, 0x1bca3],
  [0x1d173, 0x1d17a],
  [0xe0000, 0xe0fff],
];

// Control characters (C0 other than tab and line feed, DEL, C1), the line
// breaks other than line feed (U+2028, U+2029; NEL is in C1), and the
// invisible characters above.
function hex(n: number): string {
  return `\\u{${n.toString(16)}}`;
}
const classBody =
  '\\u{0}-\\u{8}\\u{b}-\\u{1f}\\u{7f}-\\u{9f}\\u{2028}\\u{2029}' +
  INVISIBLE.map(([lo, hi]) => (lo === hi ? hex(lo) : `${hex(lo)}-${hex(hi)}`)).join('');
const UNSAFE = new RegExp(`[${classBody}]`, 'u');
const UNSAFE_RUNS = new RegExp(`[${classBody}]+`, 'gu');

/** Whether `c` (one code point) must be shown as an escape. */
export function isUnsafe(c: string): boolean {
  return UNSAFE.test(c);
}

/** The escape shown for each code point of `s`: `\u{202e}`. */
export function escapeAll(s: string): string {
  let out = '';
  for (const c of s) out += `\\u{${c.codePointAt(0)!.toString(16)}}`;
  return out;
}

/** `s` with every unsafe character replaced by its escape, for places
 * that take plain text only (a `title` attribute, a joined string). */
export function visible(s: string): string {
  if (!UNSAFE.test(s)) return s;
  return s.replace(UNSAFE_RUNS, escapeAll);
}

export interface Segment {
  text: string;
  /** `text` is the escape of characters that are not shown. */
  escaped: boolean;
}

/** `s` split into plain text and escapes, for rendering the escapes in a
 * distinct style. Tabs and line feeds are kept as they are. */
export function segments(s: string): Segment[] {
  if (!UNSAFE.test(s)) return [{ text: s, escaped: false }];
  const out: Segment[] = [];
  let last = 0;
  for (const m of s.matchAll(UNSAFE_RUNS)) {
    const i = m.index;
    if (i > last) out.push({ text: s.slice(last, i), escaped: false });
    out.push({ text: escapeAll(m[0]), escaped: true });
    last = i + m[0].length;
  }
  if (last < s.length) out.push({ text: s.slice(last), escaped: false });
  return out;
}
