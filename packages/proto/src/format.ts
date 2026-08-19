/**
 * The numbers, formatted (05 §"Numbers that don't lie or twitch").
 *
 * This is the TypeScript half of `crates/vortex-proto/src/fmt.rs`, and it lives beside the
 * generated types for the same reason those do: every surface of the product — the desktop
 * list, the page overlay, the CLI, the daemon's own logs — has to render the same byte
 * count the same way. Two implementations that round differently produce "1.2 GB" in the
 * overlay and "1.29 GB" in the row for one file, and a user who notices that stops
 * trusting every other number in the app.
 *
 * `tests/format.test.ts` runs the exact case table the Rust tests run.
 *
 * Sizes are base-10 (kB, MB, GB) to match the file manager and the source page; the engine
 * computes in base-2 internally. Documented, consistent, never mixed.
 */

const UNITS = ["B", "kB", "MB", "GB", "TB", "PB"] as const;

/**
 * Base-10 size, three significant figures.
 *
 * `1_290_000_000` renders `1.29 GB`, never `1.2 GB` — showing 1.2 for a 1.29 file is the
 * kind of small lie users notice.
 */
export function bytes(n: number): string {
  const whole = Math.max(0, Math.floor(n));
  if (whole < 1000) return `${whole} B`;
  let value = whole;
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${threeSigFigs(value)} ${UNITS[unit]}`;
}

/** Throughput. Same scale as {@link bytes}, with `/s`. Zero is an em dash, not `0 B/s`. */
export function rate(bps: number): string {
  return bps <= 0 ? "—" : `${bytes(bps)}/s`;
}

/**
 * A size a manifest declared rather than one anyone measured.
 *
 * `bandwidth × duration / 8` uses a *peak* bandwidth, so it routinely lands 20–30% high.
 * The tilde is the difference between an estimate and a promise.
 */
export function estimate(n: number | null | undefined): string {
  return n === null || n === undefined ? "" : `~${bytes(n)}`;
}

/**
 * Quantized so it never changes every frame: under a minute in seconds, under an hour as
 * `4m 20s`, above that `1h 12m`. Never `3,847 seconds`.
 */
export function eta(secs: number | null | undefined): string {
  if (secs === null || secs === undefined || !Number.isFinite(secs)) return "";
  const whole = Math.max(0, Math.round(secs));
  if (whole < 60) return `${whole}s`;
  if (whole < 3600) {
    const m = Math.floor(whole / 60);
    const s = whole % 60;
    return s === 0 ? `${m}m` : `${m}m ${s}s`;
  }
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  return m === 0 ? `${h}h` : `${h}h ${m}m`;
}

/**
 * No fake precision: `62%`, not `62.4%`.
 *
 * Rounds toward zero so a job never reads 100% before it is finished — the one rounding
 * direction a progress readout is not allowed to get wrong.
 */
export function percent(completed: number, total: number | null | undefined): string {
  const fraction = ratio(completed, total);
  return fraction === null ? "—" : `${Math.floor(fraction * 100)}%`;
}

/** The same computation as {@link percent}, undecorated, for widths and bars. */
export function ratio(completed: number, total: number | null | undefined): number | null {
  if (!total || total <= 0) return null;
  return Math.min(1, Math.max(0, completed / total));
}

/** `2537` → `42:17`. Hours only when there are hours. Used for media durations. */
export function duration(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return "";
  const whole = Math.max(0, Math.round(seconds));
  const s = whole % 60;
  const m = Math.floor(whole / 60) % 60;
  const h = Math.floor(whole / 3600);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

function threeSigFigs(v: number): string {
  const s = v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2);
  // A trailing zero in a live readout twitches as much as a proportional digit does.
  return s.includes(".") ? s.replace(/0+$/, "").replace(/\.$/, "") : s;
}
