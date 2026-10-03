// Fixed-height row virtualization.

export interface VirtualWindow {
  start: number;
  end: number; // exclusive
  padTop: number;
  padBottom: number;
}

export function virtualWindow(
  scrollTop: number,
  viewport: number,
  rowHeight: number,
  total: number,
  overscan = 8,
): VirtualWindow {
  if (total <= 0 || rowHeight <= 0) return { start: 0, end: 0, padTop: 0, padBottom: 0 };
  const first = Math.floor(Math.max(0, scrollTop) / rowHeight);
  const visible = Math.ceil(Math.max(viewport, rowHeight) / rowHeight) + 1;
  const start = Math.max(0, Math.min(total - 1, first - overscan));
  const end = Math.min(total, first + visible + overscan);
  return { start, end, padTop: start * rowHeight, padBottom: (total - end) * rowHeight };
}

/** The scrollTop that brings row `index` into view (unchanged if visible). */
export function scrollToRow(index: number, scrollTop: number, viewport: number, rowHeight: number): number {
  const top = index * rowHeight;
  if (top < scrollTop) return top;
  if (top + rowHeight > scrollTop + viewport) return top + rowHeight - viewport;
  return scrollTop;
}
