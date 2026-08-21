/**
 * The Vortex mark, and the three places it lives.
 *
 *   node scripts/logo.mjs
 *
 * Eight arms of a snowflake, each one pulled into a curl. That is the whole idea: the
 * still figure is a file, the spin is eight workers pulling at it, and the two are the
 * same drawing. Eight because eight is the number in this product — the worker spectrum in
 * `packages/tokens` has eight lanes and the segment map has eight colours, so a mark with
 * six arms would be a mark about nothing.
 *
 * Each arm is three strokes on one construction:
 *
 *   the stem     a radius from the centre out to `r1`
 *   the branches two short lines leaving the stem at `rb`, at ±`beta`
 *   the curl     an arc that carries the arm round, off-centre by `cd` degrees
 *
 * It is generated rather than drawn because every number above is load-bearing and none of
 * them is editable by hand: nudging the branch angle by two degrees moves eight curls, and
 * the difference between "snowflake" and "flower" is about six units of arc radius. A
 * designer would call this a construction drawing; here it is forty lines of geometry and
 * the SVG is the artefact.
 *
 * The mark is the one thing in the product that is allowed to carry colour outside the
 * data (05 §The one rule), and it spends that licence on exactly one hue: `--w4` azure,
 * which the tokens file calls "the resting state of the family: the hue a finished byte
 * settles at". White on azure, nothing else.
 */

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

const D = Math.PI / 180;
const C = 512;

/** @typedef {{ x: number, y: number }} Point */

/** @param {number} a @returns {Point} */
const u = (a) => ({ x: Math.cos(a), y: Math.sin(a) });
/** @param {number} x */
const n = (x) => String(Math.round(x * 10) / 10);
/** @param {Point} p @param {Point} d @param {number} k @returns {Point} */
const step = (p, d, k) => ({ x: p.x + d.x * k, y: p.y + d.y * k });
/** @param {Point} a @param {Point} b */
const line = (a, b) => `M${n(a.x)} ${n(a.y)}L${n(b.x)} ${n(b.y)}`;

/** `--w4` in the light theme. The icon is one file, so it cannot follow the theme. */
export const AZURE = "#206ae1";
/** Matches the 8 px corner the window itself draws, scaled to a 1024 tile. */
export const PLATE_RADIUS = 228;

export const MARK = {
  arms: 8,
  /** How far the stem reaches. */
  r1: 196,
  /** Where the two branches leave the stem, and how far and how wide they go. */
  rb: 140,
  beta: 24,
  prong: 100,
  /** The curl: an arc of `rad`, centred `cr` from the middle and `cd` degrees off the arm. */
  cr: 250,
  cd: 12,
  rad: 104,
  start: 328,
  span: 140,
  /** One stroke width for every line in the mark. There is no second weight. */
  w: 50,
  /** An arm pointing straight up, so the figure sits square in its tile. */
  phase: -90,
};

/** @typedef {typeof MARK} Mark */

/**
 * The mark, as three groups in drawing order.
 *
 * The grouping is not cosmetic: the boot splash draws them in this order, so the figure
 * assembles as a snowflake and only then starts to turn.
 *
 * @param {Mark} m
 */
export function parts(m = MARK) {
  const middle = { x: C, y: C };
  const turn = 360 / m.arms;
  /** @type {string[]} */ const spokes = [];
  /** @type {string[]} */ const branches = [];
  /** @type {string[]} */ const curls = [];

  for (let k = 0; k < m.arms; k++) {
    const a = (m.phase + k * turn) * D;
    const along = u(a);
    spokes.push(line(middle, step(middle, along, m.r1)));

    const foot = step(middle, along, m.rb);
    for (const side of [1, -1]) {
      branches.push(line(foot, step(foot, u(a + side * m.beta * D), m.prong)));
    }

    const centre = step(middle, u((m.phase + m.cd + k * turn) * D), m.cr);
    const from = (m.phase + m.start + k * turn) * D;
    const segments = Math.ceil(m.span / 90);
    const h = (m.span * D) / segments;
    let d = `M${n(centre.x + m.rad * Math.cos(from))} ${n(centre.y + m.rad * Math.sin(from))}`;
    for (let i = 1; i <= segments; i++) {
      const g = u(from + i * h);
      d += `A${n(m.rad)} ${n(m.rad)} 0 0 1 ${n(centre.x + m.rad * g.x)} ${n(centre.y + m.rad * g.y)}`;
    }
    curls.push(d);
  }

  return { spokes, branches, curls };
}

/**
 * Every stroke as one `d`, in draw order.
 *
 * @param {Mark} m
 */
export function figure(m = MARK) {
  const { spokes, branches, curls } = parts(m);
  return [...spokes, ...branches, ...curls].join("");
}

/**
 * The mark at chrome size.
 *
 * The icon, not a reduction of it: the same azure plate and the same white figure that the
 * taskbar shows, drawn at 22 px in the corner of the titlebar.
 *
 * The figure alone does not survive that size — twenty-four hairlines inside twenty pixels
 * is a smudge — but the figure *on its plate* does, and for the reason the plate exists:
 * white on saturated azure is the highest-contrast pair in the product, so the strokes hold
 * their shape at a weight that would disappear against paper. Which is why this is 22 rather
 * than the 20 the rest of the titlebar is built on. Two pixels is the difference between a
 * legible rosette and a blue square with something in it.
 */
export const SMALL = { size: 22 };

/** @param {Mark} m */
const svg = (m = MARK) => `<!--
  The Vortex mark. Generated by scripts/logo.mjs — edit the numbers there, not here.

  Eight snowflake arms pulled into a curl: the file, and the eight workers pulling at it.
  Azure is the w4 token, which tokens.css calls the hue a finished byte settles at, and it
  is the only colour the mark uses.

  This file is the source; the png, ico and icns beside it are generated from it by
  \`npm run tauri icon src-tauri/icons/vortex.svg\`. (No double hyphen anywhere in this
  comment: XML forbids it, and the SVG rasteriser refuses the file rather than warning.)
-->
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">
  <rect width="1024" height="1024" rx="${PLATE_RADIUS}" fill="${AZURE}" />
  <path
    d="${figure(m)}"
    fill="none"
    stroke="#ffffff"
    stroke-width="${m.w}"
    stroke-linecap="round"
  />
</svg>
`;

/**
 * The splash markup, inlined into `index.html`.
 *
 * It has to be inline — it paints before the app bundle is parsed, which on a cold start
 * is the longest blank stretch there is — so this is the price: one generated block in a
 * hand-written file, between two sentinels, and a test that fails if it drifts from the
 * icon.
 *
 * @param {Mark} m
 */
function splash(m = MARK) {
  const { spokes, branches, curls } = parts(m);
  /** @param {string} name @param {string[]} ds */
  const group = (name, ds) =>
    `        <path class="mark-${name}" pathLength="1" d="${ds.join("")}" />`;
  return [
    `      <svg class="mark" width="112" height="112" viewBox="0 0 1024 1024" fill="none"`,
    `           stroke="currentColor" stroke-width="${m.w}" stroke-linecap="round" aria-hidden="true">`,
    group("spokes", spokes),
    group("branches", branches),
    group("curls", curls),
    `      </svg>`,
  ].join("\n");
}

/**
 * The titlebar mark, inlined into `Mark.svelte`.
 *
 * Third home for one drawing, and the same bargain as the splash: a generated block inside
 * a hand-written file, between two sentinels, with a test that fails when it drifts.
 *
 * The figure is in its own group so that it can turn without the plate turning with it. A
 * rotating rounded square would be a spinner; a still plate with the figure turning inside
 * it is the icon, doing the thing the icon is a picture of.
 *
 * @param {Mark} m
 */
function chrome(m = MARK) {
  return [
    `  <rect width="1024" height="1024" rx="${PLATE_RADIUS}" fill="${AZURE}" />`,
    `  <g class="figure">`,
    `    <path`,
    `      d="${figure(m)}"`,
    `      fill="none"`,
    `      stroke="#ffffff"`,
    `      stroke-width="${m.w}"`,
    `      stroke-linecap="round"`,
    `    />`,
    `  </g>`,
  ].join("\n");
}

/** @param {string} text @param {string} tag @param {string} body */
const between = (text, tag, body) => {
  const open = `<!-- ${tag} -->`;
  const close = `<!-- /${tag} -->`;
  const a = text.indexOf(open);
  const b = text.indexOf(close);
  if (a < 0 || b < 0) throw new Error(`index.html has no ${open} … ${close} block`);
  // Keep the closing sentinel at the indentation of the opening one, so the generated
  // block reads as part of the file rather than as something bolted into it.
  const pad = " ".repeat(a - (text.lastIndexOf("\n", a) + 1));
  return `${text.slice(0, a + open.length)}\n${body}\n${pad}${text.slice(b)}`;
};

// Only when run. `mark.test.ts` imports the geometry above to check that the two
// generated files still agree with it, and a test that rewrote them as a side effect of
// asking would always pass.
if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const icon = join(ROOT, "apps", "desktop", "src-tauri", "icons", "vortex.svg");
  writeFileSync(icon, svg());
  console.log(`mark   -> ${icon}`);

  const page = join(ROOT, "apps", "desktop", "index.html");
  writeFileSync(page, between(readFileSync(page, "utf8"), "mark", splash()));
  console.log(`splash -> ${page}`);

  const chip = join(ROOT, "apps", "desktop", "src", "components", "Mark.svelte");
  writeFileSync(chip, between(readFileSync(chip, "utf8"), "ring", chrome()));
  console.log(`chrome -> ${chip}`);
}
