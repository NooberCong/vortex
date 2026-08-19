/**
 * Where a badge can actually be seen (03 §The overlay).
 *
 * `getBoundingClientRect()` answers where a player *is*, which is not the same question as
 * where a badge pinned to it will be visible. Three things routinely make those differ, and
 * all three produce the same symptom — a button floating over a page, pointing at nothing:
 *
 * 1. **A clipping ancestor.** A player inside a carousel, an accordion or a fixed-height
 *    card has a rect that extends past the box that clips it. Half the player is not on the
 *    screen, and the corner the badge is pinned to is the half that is gone.
 * 2. **A sticky header.** A site's own bar is painted over the top of the viewport, and a
 *    badge in the top-right of a player scrolled under it is behind the bar.
 * 3. **Something drawn on top.** A modal, a cookie wall, a sticky player from another
 *    article. The player is exactly where it was; nobody can see it.
 *
 * None of these has an event, and all of them are expensive to measure — `getComputedStyle`
 * up an ancestor chain, `elementsFromPoint` into the page's own stacking order. So nothing
 * here runs per frame. `index.ts` takes these measurements on its repair tick and re-uses
 * the answers in between, which is the same trade IDM makes with a 200 ms debounce.
 */

/** A rectangle in viewport coordinates. Deliberately narrower than `DOMRect`. */
export interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/**
 * How far down the viewport a site's own fixed bar reaches, at most.
 *
 * A bar taller than this is not a header, it is the page, and pushing every badge below it
 * would be worse than ignoring it.
 */
const HEADER_CEILING = 0.25;

/** A bar has to span most of the window to be one. A floating chat bubble is not a header. */
const HEADER_SPAN = 0.8;

/** The visible rect of an element: its own, intersected with everything that clips it. */
export function clippedBox(element: Element, clippers: readonly Element[]): Box {
  const rect = element.getBoundingClientRect();
  let box: Box = { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
  for (const clipper of clippers) {
    if (!clipper.isConnected) continue;
    const edge = clipper.getBoundingClientRect();
    box = {
      left: Math.max(box.left, edge.left),
      top: Math.max(box.top, edge.top),
      right: Math.min(box.right, edge.right),
      bottom: Math.min(box.bottom, edge.bottom),
    };
    // Clipped out of existence. Nothing further can widen it, so stop.
    if (box.right <= box.left || box.bottom <= box.top) break;
  }
  return box;
}

/**
 * The ancestors whose `overflow` actually clips this element.
 *
 * The qualifier is the whole function. Overflow clips descendants *in its containing-block
 * chain*, and a positioned element leaves that chain: a player with `position: absolute`
 * — which is how nearly every custom player fills its container — is clipped by the nearest
 * positioned ancestor and by nothing between the two. Collecting every `overflow: hidden`
 * ancestor regardless would invent clipping the browser does not apply, and a badge hidden
 * for a reason that is not real is worse than one that is occasionally misplaced.
 *
 * `position: fixed` ends the walk outright. It is laid out against the viewport, so nothing
 * above it in the tree has any say in where it is drawn.
 */
export function clippersOf(element: Element): Element[] {
  const clippers: Element[] = [];
  // True while the walk is above an absolutely positioned box and below its containing
  // block, which is the stretch where `overflow` has no effect on us.
  let escaped = positionOf(element) === "absolute";

  for (let node = parentOf(element); node !== null; node = parentOf(node)) {
    if (node === document.documentElement || node === document.body) break;
    const style = styleOf(node);
    if (!style) break;
    const position = style.position || "static";
    if (position === "fixed") break;

    const positioned = position !== "static";
    // A positioned ancestor is the containing block of an escaped box, so it clips even
    // though the ones below it did not.
    if ((!escaped || positioned) && clips(style)) clippers.push(node);
    if (positioned) escaped = false;
    if (position === "absolute") escaped = true;
  }
  return clippers;
}

/**
 * The height of the site's own bar stuck to the top of the viewport, or `0`.
 *
 * Sampled by hit-testing the top edge rather than by looking for a header — sites agree on
 * almost nothing about what a header is called or where it sits in the tree, and they agree
 * completely on it being the thing painted at the top of the window.
 */
export function stickyHeader(ignore: Element): number {
  const stack = pointStack(innerWidth / 2, 1);
  if (stack === null) return 0;

  const ceiling = innerHeight * HEADER_CEILING;
  let deepest = 0;
  for (const element of stack) {
    if (element === ignore || ignore.contains(element)) continue;
    const style = styleOf(element);
    if (!style) continue;
    // The element that *paints* the bar is routinely not the element that *pins* it —
    // YouTube's masthead is a `position: fixed` box with a transparent background and an
    // absolutely positioned child that carries the colour. Requiring one element to do
    // both misses the bar on some of the largest sites there are.
    if (alpha(style.opacity) < 0.9 || !paints(style) || !pinned(element)) continue;

    const rect = element.getBoundingClientRect();
    // Stuck to the top, wide enough to be a bar, short enough to be a header.
    if (rect.top > 1 || rect.bottom <= 0 || rect.bottom > ceiling) continue;
    if (rect.width < innerWidth * HEADER_SPAN) continue;
    deepest = Math.max(deepest, rect.bottom);
  }
  return deepest;
}

/** Is this element, or anything it hangs from, held against the viewport? */
function pinned(element: Element): boolean {
  for (let node: Element | null = element; node !== null; node = parentOf(node)) {
    const position = styleOf(node)?.position;
    if (position === "fixed" || position === "sticky") return true;
    if (node === document.documentElement) break;
  }
  return false;
}

/**
 * Is something opaque drawn over this point, in front of the player?
 *
 * Only `background-color` counts as opaque. Every player on the web layers transparent
 * elements over its own video — a controls bar, a gradient scrim, a click-catching pane —
 * and treating those as cover would hide the badge on exactly the sites it is for. A modal
 * or a consent wall has a real background, and that is what this is looking for.
 *
 * Reaching the player, its subtree or its ancestors means nothing came between: the badge
 * is on top of the thing it is pointing at, which is the whole question.
 */
export function covered(x: number, y: number, player: Element, ignore: Element): boolean {
  const stack = pointStack(x, y);
  if (stack === null) return false;

  for (const element of stack) {
    if (element === player || element.contains(player) || player.contains(element)) return false;
    if (element === ignore || ignore.contains(element)) continue;
    const style = styleOf(element);
    if (!style) continue;
    if (style.visibility === "hidden" || alpha(style.opacity) === 0) continue;
    if (paints(style)) return true;
  }
  // The point is off the document, or the stack ran out without reaching the player.
  // Neither is evidence of anything, and hiding a badge needs evidence.
  return false;
}

// ── The awkward parts of asking a page about itself ─────────────────────────

/** The stack of elements at a point, topmost first, or `null` where that cannot be asked. */
function pointStack(x: number, y: number): Element[] | null {
  if (typeof document.elementsFromPoint !== "function") return null;
  if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
  try {
    return document.elementsFromPoint(Math.round(x), Math.round(y));
  } catch {
    return null;
  }
}

/** Up one level, out of a shadow root where there is one. */
function parentOf(node: Element): Element | null {
  if (node.parentElement) return node.parentElement;
  const root = node.getRootNode();
  return root instanceof ShadowRoot ? root.host : null;
}

function positionOf(element: Element): string {
  return styleOf(element)?.position || "static";
}

/** `getComputedStyle` throws for a node in no document, and answers nothing in some. */
function styleOf(element: Element): CSSStyleDeclaration | null {
  try {
    return getComputedStyle(element) ?? null;
  } catch {
    return null;
  }
}

/**
 * Does this box clip what is inside it? Either axis clipping clips both, per the cascade.
 *
 * The shorthand is read as well as the longhands because not every implementation of
 * `getComputedStyle` expands it — including the one the tests run against.
 */
function clips(style: CSSStyleDeclaration): boolean {
  return notVisible(style.overflowX) || notVisible(style.overflowY) || notVisible(style.overflow);
}

/** An unset value is the initial one, and the initial value of `overflow` is `visible`. */
function notVisible(overflow: string | undefined): boolean {
  return overflow !== undefined && overflow !== "" && overflow !== "visible";
}

/** Does this element put its own colour on the screen? */
function paints(style: CSSStyleDeclaration): boolean {
  return opaque(style.backgroundColor);
}

/**
 * Whether a colour covers anything behind it.
 *
 * The fourth component of `rgb()`/`rgba()` is the alpha and nothing else is — reading the
 * last number in the list would call `rgb(0, 0, 0)` transparent, which is black.
 */
function opaque(color: string | undefined): boolean {
  const value = (color ?? "").trim().toLowerCase();
  if (value === "" || value === "transparent") return false;
  const inside = /^rgba?\(([^)]*)\)$/.exec(value);
  if (!inside) return true;
  const parts = inside[1]!.split(/[,\s/]+/).filter(Boolean);
  return parts.length < 4 || parseFloat(parts[3]!) > 0;
}

/** `opacity`, defaulted to fully opaque where the page or the platform did not say. */
function alpha(opacity: string | undefined): number {
  const value = Number.parseFloat(opacity ?? "");
  return Number.isFinite(value) ? value : 1;
}
