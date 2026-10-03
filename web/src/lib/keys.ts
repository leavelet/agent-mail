// Keyboard shortcuts must never fire while the user types.

export function typing(e: KeyboardEvent): boolean {
  if (e.ctrlKey || e.metaKey || e.altKey) return true;
  const el = e.target as HTMLElement | null;
  if (!el || !el.tagName) return false;
  const tag = el.tagName.toLowerCase();
  return tag === 'input' || tag === 'textarea' || tag === 'select' || el.isContentEditable;
}

/** j/k/arrows: next index within [0, n). */
export function moveIndex(key: string, i: number, n: number): number | null {
  if (n === 0) return null;
  if (key === 'j' || key === 'ArrowDown') return Math.min(n - 1, i < 0 ? 0 : i + 1);
  if (key === 'k' || key === 'ArrowUp') return Math.max(0, i < 0 ? 0 : i - 1);
  if (key === 'Home') return 0;
  if (key === 'End') return n - 1;
  return null;
}
