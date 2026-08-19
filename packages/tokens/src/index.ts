/**
 * The one number both consumers of `tokens.css` have to agree on.
 *
 * There used to be a JavaScript copy of the eight hex values here, on the theory that a
 * canvas cannot read a CSS custom property. It can — `getComputedStyle` resolves them, and
 * the desktop app caches the answer once per theme change (`apps/desktop/src/lib/palette.ts`).
 * Since the spectrum now carries different lightness per theme, a single frozen array would
 * have been wrong half the time as well as duplicated, so it is gone.
 *
 * What remains is the count, which is genuinely shared: the renderer wraps a worker's lane
 * number into this many hues, and the stylesheet has to declare exactly that many. A test
 * checks the stylesheet against this number rather than the other way round.
 */
export const LANES = 8;

/**
 * The custom property a worker's lane draws in, wrapping rather than clamping.
 *
 * Worker numbering is 1-based and unbounded — the engine may evict worker 3 and open worker
 * 9 against a different address — so a ninth connection reuses the first hue. Two lanes
 * sharing a colour is a cosmetic collision; a lane with no colour is an invisible worker.
 */
export function laneVar(worker: number): string {
  const index = ((Math.trunc(worker) - 1) % LANES + LANES) % LANES;
  return `--w${index + 1}`;
}
