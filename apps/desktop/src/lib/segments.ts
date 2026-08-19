/**
 * The segment map's arithmetic (05 §Signature).
 *
 * A `ProgressFrame` carries run-length encoded block ownership: `(start, len, owner)`,
 * where owner 0 means complete and 1..=N means in flight by that worker. A 4 GB file at
 * 1 MiB blocks is 4096 blocks drawn into perhaps 600 device pixels, so almost every column
 * of the map covers several blocks and most runs are narrower than a pixel.
 *
 * The rule that makes this honest is one line of the spec: **the map never rounds a
 * partial block up to complete.** A column that is one-seventh downloaded is drawn at
 * one-seventh alpha, never as a solid block — because a progress display that overstates
 * is the one kind of inaccuracy users can actually catch you at, and the map's whole claim
 * is that it shows the real occupancy of the file.
 *
 * Kept pure and separate from the canvas so it can be tested, which is the only way to
 * know that claim still holds.
 */

import { LANES } from "@vortex/tokens";

/** `(start_block, len, owner)`, straight off the wire. */
export type Run = readonly [number, number, number];

/** Channel 0 is "complete"; 1..=LANES are the worker lanes. */
export const CHANNELS = LANES + 1;

export { LANES };

/**
 * Which channel a run's owner draws into.
 *
 * Worker numbering is 1-based and unbounded — the engine may evict worker 3 and open
 * worker 9 against a different address — so it wraps rather than clamps. Two lanes sharing
 * a hue is a cosmetic collision; a lane drawn outside the buffer is a crash.
 */
export function channel(owner: number): number {
  if (owner <= 0) return 0;
  return ((owner - 1) % LANES) + 1;
}

/** The buffer [`coverage`] fills, sized for a given width. Allocated once and reused. */
export function buffer(width: number): Float32Array {
  return new Float32Array(Math.max(0, width) * CHANNELS);
}

/**
 * Accumulates per-column coverage into `out`, which must be `width × CHANNELS` long.
 *
 * `out[column * CHANNELS + channel]` ends up in `[0, 1]`: the fraction of that column's
 * blocks owned by that channel. Columns are aggregated by coverage rather than sampled,
 * so a run that is a tenth of a pixel wide contributes a tenth rather than being either
 * dropped or promoted to a whole pixel — dropping loses the four workers that just started,
 * and promoting is the lie.
 */
export function coverage(
  runs: readonly Run[],
  blocks: number,
  width: number,
  out: Float32Array,
): void {
  out.fill(0);
  if (blocks <= 0 || width <= 0) return;

  const perColumn = blocks / width;
  for (const [start, len, owner] of runs) {
    if (len <= 0) continue;
    // A frame that describes blocks past the end is a daemon bug, but it must not be able
    // to write past the end of this buffer.
    const from = Math.max(0, start) / perColumn;
    const to = Math.min(blocks, start + len) / perColumn;
    if (to <= from) continue;

    const ch = channel(owner);
    const first = Math.max(0, Math.floor(from));
    const last = Math.min(width, Math.ceil(to));
    for (let column = first; column < last; column += 1) {
      const overlap = Math.min(to, column + 1) - Math.max(from, column);
      if (overlap <= 0) continue;
      const at = column * CHANNELS + ch;
      out[at] = (out[at] ?? 0) + overlap;
    }
  }

  // Ownership is exclusive, so a column can only exceed 1 through floating-point drift.
  // Clamping here means the renderer can use these values as alpha without checking.
  for (let i = 0; i < out.length; i += 1) {
    if (out[i]! > 1) out[i] = 1;
  }
}

/**
 * One worker's territory: what it has finished, what it is fetching, where it stops.
 *
 * All three in block coordinates, `from <= cursor <= to`.
 */
export interface Lease {
  /** The worker's own number, as the engine reports it. */
  owner: number;
  /** Which of the eight hues it draws in. */
  lane: number;
  from: number;
  cursor: number;
  to: number;
}

/**
 * Reconstructs each worker's lease from a frame's runs.
 *
 * A `ProgressFrame` says which blocks are complete and which are in flight *by whom* — but
 * completed blocks carry no worker, so the wire alone cannot say who fetched them. The
 * reconstruction uses what the scheduler guarantees: a lease is a contiguous forward range,
 * and a worker's cursor only moves right. The complete blocks immediately to the left of a
 * worker's in-flight run, up to the previous worker's lease, are therefore its own.
 *
 * That inference is exact for a lease nobody has stolen from, and it is what makes the
 * expanded map show `▓▓▓▓▒▒▒····` per lane instead of the same grey behind every band.
 *
 * Where it can be wrong: after a steal, the thief inherits the back half of a lease whose
 * completed head belongs to the worker it was taken from, so a stretch can be drawn in the
 * wrong hue. The *extent* is still exactly right — no block is invented, dropped or moved —
 * and the expanded row numbers every lane and states its speed in words, so nothing depends
 * on the colour alone.
 */
export function leases(runs: readonly Run[]): Lease[] {
  const ordered = [...runs].filter(([, len]) => len > 0).sort((a, b) => a[0] - b[0]);
  const out: Lease[] = [];
  let claimed = 0;

  for (let i = 0; i < ordered.length; i += 1) {
    const [start, len, owner] = ordered[i]!;
    if (owner <= 0) continue;

    // Walk left through complete runs that touch this one, stopping at the previous
    // worker's territory.
    let from = start;
    for (let j = i - 1; j >= 0; j -= 1) {
      const [before, length, who] = ordered[j]!;
      if (who !== 0 || before + length !== from || before < claimed) break;
      from = before;
    }

    out.push({ owner, lane: channel(owner), from, cursor: start, to: start + len });
    claimed = start + len;
  }

  // By lane, so a band belongs to the same worker from one frame to the next. Sorting by
  // position would make every steal shuffle the rows out from under the reader.
  return out.sort((a, b) => a.owner - b.owner);
}
