// Prefix-sum index over variable row heights, backed by a Fenwick (binary
// indexed) tree.
//
// Why not a plain running sum: streaming and measurement both mutate heights
// one row at a time. A flat prefix-sum array makes every mutation O(n) (5000
// rows × 60 fps is 300k additions per second just to keep offsets), while a
// Fenwick tree gives an O(log n) point update, an O(log n) prefix query and an
// O(log n) "which row is at this pixel" search. Structural changes (prepend a
// page of older events) rebuild once in O(n), which is the right trade: those
// happen per page, not per frame.

/** A row height in CSS pixels; always finite and >= 0. */
function sanitizeHeight(height: number): number {
  if (!Number.isFinite(height) || height < 0) return 0;
  return height;
}

export class OffsetIndex {
  /** heights[i] is row i's height. */
  private heights: number[] = [];
  /** Fenwick tree, 1-based: tree[k] covers a range ending at row k-1. */
  private tree: number[] = [];
  /** Allocated Fenwick slots (>= heights.length); append grows by doubling. */
  private capacity = 0;
  private total = 0;

  constructor(heights: readonly number[] = []) {
    this.rebuild(heights);
  }

  get length(): number {
    return this.heights.length;
  }

  /** Total height of every row, in pixels. */
  totalHeight(): number {
    return this.total;
  }

  heightAt(index: number): number {
    return this.heights[index] ?? 0;
  }

  /** Height of rows [0, index). */
  offsetOf(index: number): number {
    const i = Math.max(0, Math.min(index, this.heights.length));
    let sum = 0;
    for (let k = i; k > 0; k -= k & -k) sum += this.tree[k] ?? 0;
    return sum;
  }

  /**
   * Index of the row containing `offset` (clamped to the last row), i.e. the
   * largest i with offsetOf(i) <= offset.
   */
  indexAtOffset(offset: number): number {
    if (this.heights.length === 0) return 0;
    if (offset <= 0) return 0;
    if (offset >= this.total) return this.heights.length - 1;
    let pos = 0;
    let remaining = offset;
    // Largest power of two <= length, then walk down: classic Fenwick search.
    let step = 1;
    while (step * 2 <= this.heights.length) step *= 2;
    for (; step > 0; step = Math.floor(step / 2)) {
      const next = pos + step;
      if (next <= this.heights.length) {
        const covered = this.tree[next] ?? 0;
        if (covered <= remaining) {
          pos = next;
          remaining -= covered;
        }
      }
    }
    return Math.min(pos, this.heights.length - 1);
  }

  /** Replace all heights. O(n); used on structural change (prepend/reset). */
  rebuild(heights: readonly number[]): void {
    this.heights = heights.map(sanitizeHeight);
    const n = this.heights.length;
    this.capacity = Math.max(16, n);
    this.tree = new Array<number>(this.capacity + 1).fill(0);
    this.total = 0;
    // O(n) Fenwick build: push each node's value up into its parent.
    for (let i = 1; i <= n; i += 1) {
      const h = this.heights[i - 1] ?? 0;
      this.total += h;
      this.tree[i] = (this.tree[i] ?? 0) + h;
      const parent = i + (i & -i);
      if (parent <= this.capacity) {
        this.tree[parent] = (this.tree[parent] ?? 0) + (this.tree[i] ?? 0);
      }
    }
  }

  /** Change one row's height. O(log n). Returns the height delta. */
  setHeight(index: number, height: number): number {
    if (index < 0 || index >= this.heights.length) return 0;
    const next = sanitizeHeight(height);
    const delta = next - (this.heights[index] ?? 0);
    if (delta === 0) return 0;
    this.heights[index] = next;
    this.total += delta;
    for (let k = index + 1; k <= this.capacity; k += k & -k) {
      this.tree[k] = (this.tree[k] ?? 0) + delta;
    }
    return delta;
  }

  /** Append one row. Amortized O(log n); O(n) only when the tree grows. */
  append(height: number): void {
    const h = sanitizeHeight(height);
    const n = this.heights.length;
    if (n + 1 > this.capacity) {
      // Grow: rebuild is O(n) but happens at most log2(n) times per session.
      this.rebuild([...this.heights, h]);
      return;
    }
    this.heights.push(h);
    this.total += h;
    for (let k = n + 1; k <= this.capacity; k += k & -k) {
      this.tree[k] = (this.tree[k] ?? 0) + h;
    }
  }
}