import { describe, expect, it } from "vitest";

import { bytes, duration, estimate, eta, percent, ratio } from "../src/format";

/**
 * The case tables here are lifted verbatim from `crates/vortex-proto/src/fmt.rs`.
 *
 * That is the point: this file exists to catch the day the two halves drift, and it can
 * only do that if it asserts the same inputs. If a case is changed on one side, change it
 * on the other in the same commit — a formatter that disagrees with itself across
 * processes is worse than one that is simply wrong, because only one of the two surfaces
 * ever gets reported.
 */
describe("sizes", () => {
  it("are base-ten with three significant figures", () => {
    expect(bytes(0)).toBe("0 B");
    expect(bytes(999)).toBe("999 B");
    expect(bytes(1000)).toBe("1 kB");
    expect(bytes(1_290_000_000)).toBe("1.29 GB");
    expect(bytes(4_900_000_000)).toBe("4.9 GB");
    expect(bytes(123_400_000)).toBe("123 MB");
    expect(bytes(18_200_000)).toBe("18.2 MB");
  });

  it("never render a negative", () => {
    // A partial-content accounting slip should read as zero, not as "-3 B".
    expect(bytes(-1)).toBe("0 B");
  });

  it("mark a declared size with a tilde and leave a missing one blank", () => {
    expect(estimate(1_290_000_000)).toBe("~1.29 GB");
    expect(estimate(null)).toBe("");
    expect(estimate(undefined)).toBe("");
  });
});

describe("eta", () => {
  it("is quantized", () => {
    expect(eta(41)).toBe("41s");
    expect(eta(260)).toBe("4m 20s");
    expect(eta(300)).toBe("5m");
    expect(eta(4320)).toBe("1h 12m");
    expect(eta(7200)).toBe("2h");
  });

  it("is blank when the engine cannot say", () => {
    // An unknown ETA is an empty field, never "0s" — which reads as "about to finish".
    expect(eta(null)).toBe("");
    expect(eta(Infinity)).toBe("");
  });
});

describe("percent", () => {
  it("never overstates", () => {
    expect(percent(624, 1000)).toBe("62%");
    expect(percent(999, 1000)).toBe("99%");
    expect(percent(1000, 1000)).toBe("100%");
  });

  it("is an em dash when the total is unknown", () => {
    // A chunked response has no Content-Length. The bar has no width to be, and saying
    // "0%" would be a claim the daemon never made.
    expect(percent(0, 0)).toBe("—");
    expect(percent(500, null)).toBe("—");
    expect(ratio(500, null)).toBeNull();
  });
});

describe("duration", () => {
  it("shows hours only when there are hours", () => {
    expect(duration(2537)).toBe("42:17");
    expect(duration(45)).toBe("0:45");
    expect(duration(3661)).toBe("1:01:01");
    expect(duration(null)).toBe("");
  });
});
