// Collect keys and flush them at most once per interval (live updates are
// merged into about one repaint per second).

export class Coalescer<K> {
  private keys = new Set<K>();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private last = 0;
  private interval: number;
  private flush: (keys: Set<K>) => void;
  private now: () => number;

  constructor(
    interval: number,
    flush: (keys: Set<K>) => void,
    now: () => number = () => Date.now(),
  ) {
    this.interval = interval;
    this.flush = flush;
    this.now = now;
  }

  add(key: K): void {
    this.keys.add(key);
    if (this.timer !== null) return;
    const wait = Math.max(0, this.last + this.interval - this.now());
    this.timer = setTimeout(() => this.run(), wait);
  }

  /** Flush now (e.g. on teardown). */
  run(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    if (this.keys.size === 0) return;
    const keys = this.keys;
    this.keys = new Set();
    this.last = this.now();
    this.flush(keys);
  }

  cancel(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.keys.clear();
  }

  get size(): number {
    return this.keys.size;
  }
}
