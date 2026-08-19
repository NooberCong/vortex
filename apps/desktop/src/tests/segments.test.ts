import { describe, expect, it } from "vitest";

import {
  buffer,
  CHANNELS,
  channel,
  coverage,
  LANES,
  leases,
  type Run,
} from "$lib/segments";

/** Reads one column's channel out of the flat buffer. */
const at = (cov: Float32Array, column: number, ch: number) => cov[column * CHANNELS + ch]!;

describe("coverage", () => {
  it("never rounds a partial block up to complete", () => {
    // The map's whole claim is that it shows the real occupancy of the file. One block of
    // seven in a column is one seventh of a column, and drawing it solid would be the one
    // kind of inaccuracy a user can catch you at.
    const cov = buffer(1);
    coverage([[0, 1, 0]], 7, 1, cov);
    expect(at(cov, 0, 0)).toBeCloseTo(1 / 7, 5);
  });

  it("aggregates runs narrower than a device pixel rather than dropping them", () => {
    // Twelve workers that have just opened hold one block each. Sampling would show an
    // empty bar; aggregating shows twelve twelfths of a column in flight, which is true.
    const cov = buffer(1);
    const runs: Run[] = Array.from({ length: 12 }, (_, i) => [i, 1, i + 1]);
    coverage(runs, 96, 1, cov);
    let inFlight = 0;
    for (let ch = 1; ch < CHANNELS; ch += 1) inFlight += at(cov, 0, ch);
    expect(inFlight).toBeCloseTo(12 / 96, 5);
  });

  it("splits a run across the columns it actually spans", () => {
    const cov = buffer(4);
    // Blocks 0..49 of 100, into four columns of 25 blocks each: two full, two empty.
    coverage([[0, 50, 0]], 100, 4, cov);
    expect(at(cov, 0, 0)).toBeCloseTo(1, 5);
    expect(at(cov, 1, 0)).toBeCloseTo(1, 5);
    expect(at(cov, 2, 0)).toBeCloseTo(0, 5);
    expect(at(cov, 3, 0)).toBeCloseTo(0, 5);
  });

  it("clamps a frame that describes blocks past the end", () => {
    // A daemon bug must not be able to write past the end of the buffer, and must not be
    // able to claim more of the file than the file has.
    const cov = buffer(4);
    coverage([[0, 1000, 0]], 10, 4, cov);
    for (let column = 0; column < 4; column += 1) expect(at(cov, column, 0)).toBeCloseTo(1, 5);
    expect(cov).toHaveLength(4 * CHANNELS);
  });

  it("is a no-op for a job with no block map", () => {
    const cov = buffer(4);
    cov.fill(0.5);
    coverage([], 0, 4, cov);
    expect([...cov].every((v) => v === 0)).toBe(true);
  });
});

describe("lane colour", () => {
  it("wraps rather than running out on a fast origin", () => {
    // Twenty-four connections is ordinary for a CDN. Two lanes sharing a hue is a cosmetic
    // collision; a lane drawn outside the buffer is a crash.
    expect(channel(0)).toBe(0);
    expect(channel(1)).toBe(1);
    expect(channel(LANES)).toBe(LANES);
    expect(channel(LANES + 1)).toBe(1);
    expect(channel(24)).toBe(LANES);
  });
});

describe("leases", () => {
  it("gives a worker the completed blocks immediately behind its cursor", () => {
    // The wire says which blocks are done but not who did them. A lease is contiguous and
    // a cursor only moves right, so the complete stretch touching a worker's in-flight run
    // is that worker's — which is what makes each band show `▓▓▓▒▒▒····`.
    const [lease] = leases([
      [0, 40, 0],
      [40, 10, 1],
    ]);
    expect(lease).toEqual({ owner: 1, lane: 1, from: 0, cursor: 40, to: 50 });
  });

  it("stops at the previous worker's territory", () => {
    // Worker 2's completed head must not swallow worker 1's lease.
    const found = leases([
      [0, 10, 0],
      [10, 5, 1],
      [15, 20, 0],
      [35, 5, 2],
    ]);
    expect(found).toEqual([
      { owner: 1, lane: 1, from: 0, cursor: 10, to: 15 },
      { owner: 2, lane: 2, from: 15, cursor: 35, to: 40 },
    ]);
  });

  it("does not reach across a gap", () => {
    // A stretch of untouched file between a complete region and a worker belongs to
    // neither. Extending through it would invent progress.
    const [lease] = leases([
      [0, 10, 0],
      [50, 5, 3],
    ]);
    expect(lease?.from).toBe(50);
  });

  it("orders by worker so bands do not shuffle when a lease is stolen", () => {
    // Sorting by position would move every row out from under the reader at the exact
    // moment the interesting thing happens.
    const found = leases([
      [80, 5, 1],
      [10, 5, 3],
      [40, 5, 2],
    ]);
    expect(found.map((l) => l.owner)).toEqual([1, 2, 3]);
  });

  it("ignores completed-only and empty frames", () => {
    expect(leases([[0, 100, 0]])).toEqual([]);
    expect(leases([])).toEqual([]);
    expect(leases([[0, 0, 1]])).toEqual([]);
  });
});
