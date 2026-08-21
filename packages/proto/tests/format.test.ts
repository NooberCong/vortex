import { describe, expect, it } from "vitest";

import { bytes, duration, estimate, eta, moment, percent, ratio, took, when } from "../src/format";

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

/**
 * The moments, which have no Rust counterpart — see the note above them in `format.ts`.
 *
 * Every case pins a locale. Without one these assert whatever the machine running them
 * happens to be set to, which is the definition of a test that passes here and fails in CI.
 */
describe("when", () => {
  const now = new Date(2025, 8, 10, 16, 30, 0);
  const unix = (y: number, month: number, d: number, h = 0, min = 0) =>
    Math.floor(new Date(y, month, d, h, min).getTime() / 1000);
  const say = (u: number) => when(u, now, "en-GB");

  it("says both halves, always", () => {
    // The whole point: never the time without the day, never the day without the time. A
    // reader should not have to infer either from where the row sits in the list.
    expect(say(unix(2025, 8, 10, 14, 32))).toBe("10 Sept, 14:32");
    expect(say(unix(2025, 8, 9, 23, 59))).toBe("9 Sept, 23:59");
    expect(say(unix(2025, 0, 3, 9, 11))).toBe("3 Jan, 09:11");
  });

  it("keeps one shape, so the column stays a column", () => {
    // Today and eight months ago are formatted identically. There is no ladder to fall off.
    const today = say(unix(2025, 8, 10, 9, 5));
    const older = say(unix(2025, 0, 3, 9, 5));
    expect(today.replace(/\d+ \w+/, "")).toBe(older.replace(/\d+ \w+/, ""));
  });

  it("adds the year only once there is a different one to add", () => {
    expect(say(unix(2025, 8, 4, 9, 11))).not.toContain("2025");
    expect(say(unix(2024, 8, 3, 9, 11))).toBe("3 Sept 2024, 09:11");
  });

  it("follows the machine's clock, not ours", () => {
    // Same instant, two locales: the ordering and the twelve-hour clock are the system's.
    const u = unix(2025, 8, 10, 14, 32);
    expect(when(u, now, "en-US")).toMatch(/Sep 10, 0?2:32\s?PM/);
    expect(when(u, now, "en-GB")).toBe("10 Sept, 14:32");
  });

  it("says nothing when there is nothing to say", () => {
    expect(say(NaN)).toBe("");
    expect(when(null, now, "en-GB")).toBe("");
    expect(when(undefined, now, "en-GB")).toBe("");
  });
});

describe("moment", () => {
  it("recovers everything the short form abbreviated away", () => {
    const u = Math.floor(new Date(2024, 8, 3, 9, 11).getTime() / 1000);
    const full = moment(u, "en-GB");
    expect(full).toContain("2024");
    expect(full).toMatch(/09:11/);
  });

  it("says nothing when there is nothing to say", () => {
    expect(moment(null)).toBe("");
  });
});

describe("took", () => {
  it("is the same quantisation as eta, because it is the same kind of number", () => {
    expect(took(1000, 1000 + 42)).toBe(eta(42));
    expect(took(1000, 1000 + 260)).toBe("4m 20s");
    expect(took(1000, 1000 + 4320)).toBe("1h 12m");
  });

  it("refuses a span that is not one", () => {
    expect(took(1000, null)).toBe("");
    expect(took(1000, undefined)).toBe("");
    // A clock that went backwards, not a download that took negative time.
    expect(took(1000, 900)).toBe("");
  });
});
