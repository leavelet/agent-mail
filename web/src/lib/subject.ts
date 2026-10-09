// Subjects: the server accepts up to 4,096 characters (Unicode code
// points). One-line places (list rows, the thread title, notifications)
// show the first 200 and `…`, as the command line and injected mail do;
// the message itself shows all of it.

/** The server's subject limit (`limits::MAX_SUBJECT_CHARS`). */
export const MAX_SUBJECT_CHARS = 4096;
/** Characters of a subject shown on one line (`limits::SUBJECT_SHOWN_CHARS`). */
export const SUBJECT_SHOWN_CHARS = 200;

/** Length in code points, as the server counts (not UTF-16 units). */
export function subjectLength(s: string): number {
  let n = 0;
  for (const _ of s) n++;
  return n;
}

/** `s` cut to `max` code points, followed by `…` when something was left out. */
export function shortSubject(s: string, max = SUBJECT_SHOWN_CHARS): string {
  if (s.length <= max) return s;
  const cps = Array.from(s);
  return cps.length <= max ? s : `${cps.slice(0, max).join('')}…`;
}
