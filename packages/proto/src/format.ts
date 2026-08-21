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

/* ── Moments ─────────────────────────────────────────────────────────────────
 *
 * Unlike everything above these are not shared with `fmt.rs`. The daemon logs and the CLI
 * print instants in full, because a log line has to be unambiguous a week later on someone
 * else's machine; a row in a list has to be *scannable*, which is a different job and
 * usually a shorter string. Nothing here has a Rust counterpart to disagree with.
 *
 * `locale` is a parameter with no default rather than a hard-coded format so that the
 * clock follows the machine's — a desktop app that shows 14:32 to somebody whose system
 * says 2:32 PM has decided it knows better. Tests pass one explicitly; nothing else does.
 */

/** A `JobView` timestamp, which the daemon sends as whole seconds. */
const at = (unix: number): Date => new Date(unix * 1000);

/**
 * A moment, as a date and a time.
 *
 * ```
 *   this year      3 Sep, 14:32
 *   before that    3 Sep 2024, 14:32
 * ```
 *
 * Both halves, on every row, in the same shape every time. An earlier version of this was a
 * ladder — the clock time for today, then `Yesterday`, then a weekday, then a date — on the
 * theory that each rung should say only as much as it took to separate a row from its
 * neighbours. It reads well and it answers the wrong question: a row saying `Yesterday` has
 * told you which day and taken the time away, and a row saying `14:32` has told you the time
 * and made you work out the day. The one thing a stamp in a list is for is being *the* fact,
 * whole, without the reader having to reconstruct the other half from the row's position.
 *
 * The year is the single exception, and it is not a rung: it appears when it is a different
 * year, which is the only time it distinguishes anything. Everything else is a fixed shape,
 * so the column stays a column.
 *
 * The ordering, the separator and the twelve-or-twenty-four-hour clock are the machine's,
 * not ours — a desktop app that shows `14:32` to somebody whose system says `2:32 PM` has
 * decided it knows better.
 */
export function when(
  unix: number | null | undefined,
  now: Date = new Date(),
  locale?: string | string[],
): string {
  if (unix === null || unix === undefined || !Number.isFinite(unix)) return "";
  const then = at(unix);
  return then.toLocaleString(locale, {
    day: "numeric",
    month: "short",
    ...(then.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * The same moment, in full. For the expanded row, and for the tooltip on the short one.
 *
 * Every abbreviation {@link when} makes is recoverable here, which is the only thing that
 * makes the abbreviating safe.
 */
export function moment(
  unix: number | null | undefined,
  locale?: string | string[],
): string {
  if (unix === null || unix === undefined || !Number.isFinite(unix)) return "";
  return at(unix).toLocaleString(locale, { dateStyle: "medium", timeStyle: "short" });
}

/**
 * How long something took, quantised exactly like {@link eta}.
 *
 * The same shape for time-until and time-taken on purpose: `4m 20s` means four minutes and
 * twenty seconds wherever it appears, and a second duration format would be a second thing
 * to learn to read.
 *
 * Empty when the pair does not describe a finished span — no end, or an end before the
 * start, which is a clock that went backwards rather than a download that took negative
 * time.
 */
export function took(from: number, to: number | null | undefined): string {
  if (to === null || to === undefined || !Number.isFinite(to) || !Number.isFinite(from)) {
    return "";
  }
  if (to < from) return "";
  return eta(to - from);
}
