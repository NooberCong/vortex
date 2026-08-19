import { describe, expect, it } from "vitest";

import { band, ease, paint, type MapView } from "$lib/paint";
import type { Palette } from "$lib/palette";
import { buffer, coverage, leases, CHANNELS } from "$lib/segments";

/**
 * A canvas context that writes down what it was asked to do.
 *
 * The two things worth being sure of about this renderer are both claims about draw calls
 * rather than about pixels — a partial column is filled at partial alpha, and a wide run
 * costs one fill — so recording the calls is a more direct test than rasterising and
 * sampling would be, and it does not need a real canvas.
 */
interface Fill {
  x: number;
  y: number;
  w: number;
  h: number;
  alpha: number;
  style: unknown;
}

function recorder() {
  const fills: Fill[] = [];
  const ctx = {
    globalAlpha: 1,
    globalCompositeOperation: "source-over",
    fillStyle: "" as unknown,
    shadowColor: "",
    shadowBlur: 0,
    save() {},
    restore() {},
    beginPath() {},
    clip() {},
    roundRect() {},
    clearRect() {},
    createLinearGradient: () => ({ addColorStop() {} }),
    fillRect(x: number, y: number, w: number, h: number) {
      fills.push({ x, y, w, h, alpha: ctx.globalAlpha, style: ctx.fillStyle });
    },
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, fills };
}

const palette: Palette = {
  channels: ["C", "L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8"],
  track: "TRACK",
  attention: "RED",
  text: "TEXT",
  dim: "DIM",
};

/** The data fills only — not the track underneath and not the sheen on top. */
const data = (fills: Fill[]) =>
  fills.filter((f) => typeof f.style === "string" && f.style !== "TRACK");

describe("the collapsed bar", () => {
  it("draws a half-covered column at half alpha, never at one", () => {
    const width = 4;
    const cov = buffer(width);
    // Eight blocks into four columns; block 0 and half of column 1 are complete.
    coverage([[0, 3, 0]], 8, width, cov);
    const { ctx, fills } = recorder();
    paint(ctx, view({ kind: "coverage", cov }, width, 6));

    const complete = data(fills).filter((f) => f.style === "C");
    expect(complete.length).toBeGreaterThan(0);
    const partial = complete.find((f) => f.alpha < 0.99);
    expect(partial, "a partially covered column must be drawn at partial alpha").toBeDefined();
    expect(Math.max(...complete.map((f) => f.alpha))).toBeLessThanOrEqual(1);
  });

  it("costs one fill per run, not one per column", () => {
    // Forty runs across a 1200-device-pixel bar, twenty times a second, in front of forty
    // rows. Coalescing is the difference between that and 48,000 fills a second.
    const width = 1200;
    const cov = buffer(width);
    coverage(
      Array.from({ length: 20 }, (_, i) => [i * 100, 50, i % 2] as const),
      2000,
      width,
      cov,
    );
    const { ctx, fills } = recorder();
    paint(ctx, view({ kind: "coverage", cov }, width, 6));
    expect(data(fills).length).toBeLessThan(80);
  });

  it("paints a track under everything, so an empty job is still a bar", () => {
    const { ctx, fills } = recorder();
    paint(ctx, view({ kind: "coverage", cov: buffer(10) }, 10, 6));
    expect(fills[0]).toMatchObject({ x: 0, y: 0, w: 10, style: "TRACK" });
  });
});

describe("the plain bar", () => {
  it("is exactly as wide as the fraction it was given", () => {
    const { ctx, fills } = recorder();
    paint(ctx, view({ kind: "bar", done: 0.28 }, 200, 6));
    expect(fills[1]?.style).toBe("C");
    expect(fills[1]?.x).toBe(0);
    expect(fills[1]?.w).toBeCloseTo(56, 6);
  });

  it("clamps rather than overflowing", () => {
    const { ctx, fills } = recorder();
    paint(ctx, view({ kind: "bar", done: 4 }, 200, 6));
    expect(fills[1]?.w).toBe(200);
  });
});

describe("the lane map", () => {
  const runs = [
    [0, 40, 0],
    [40, 10, 1],
    [50, 30, 0],
    [80, 20, 2],
  ] as const;

  it("draws each worker's past solid and its present lighter, in its own hue", () => {
    const { ctx, fills } = recorder();
    const found = leases(runs);
    paint(
      ctx,
      view({ kind: "lanes", leases: found, blocks: 100, shatter: [], now: null }, 100, 14),
    );

    const lane1 = data(fills).filter((f) => f.style === "L1");
    expect(lane1).toHaveLength(2);
    expect(lane1[0]).toMatchObject({ x: 0, w: 40, alpha: 1 });
    expect(lane1[1]!.alpha).toBeLessThan(1);
    expect(lane1[1]).toMatchObject({ x: 40, w: 10 });
    expect(data(fills).some((f) => f.style === "L2")).toBe(true);
  });

  it("gives every worker a band of equal height once the shatter has settled", () => {
    const { ctx, fills } = recorder();
    paint(
      ctx,
      view({ kind: "lanes", leases: leases(runs), blocks: 100, shatter: [1, 1], now: null }, 100, 14),
    );
    const heights = new Set(data(fills).map((f) => f.h));
    expect(heights).toEqual(new Set([7]));
  });
});

describe("the shatter", () => {
  it("starts as one full-height bar and ends as stacked bands", () => {
    expect(band(0, 4, 28, 0)).toEqual({ y: 0, h: 28 });
    expect(band(3, 4, 28, 0)).toEqual({ y: 0, h: 28 });
    expect(band(0, 4, 28, 1)).toEqual({ y: 0, h: 7 });
    expect(band(3, 4, 28, 1)).toEqual({ y: 21, h: 7 });
  });

  it("uses the app's enter curve, pinned at both ends", () => {
    expect(ease(0)).toBeCloseTo(0, 5);
    expect(ease(1)).toBeCloseTo(1, 5);
    // `cubic-bezier(.2, 0, 0, 1)` front-loads: half the time is well past half the distance.
    expect(ease(0.5)).toBeGreaterThan(0.75);
    expect(ease(-3)).toBe(0);
    expect(ease(1)).toBe(1);
    expect(ease(9)).toBeCloseTo(1, 5);
  });
});

function view(
  kind: Extract<MapView, { kind: string }> extends never ? never : Partial<MapView>,
  width: number,
  height: number,
): MapView {
  return { width, height, palette, radius: 3, ...kind } as MapView;
}

describe("the buffer", () => {
  it("is sized for every lane plus complete", () => {
    expect(buffer(10)).toHaveLength(10 * CHANNELS);
    expect(buffer(-5)).toHaveLength(0);
  });
});
