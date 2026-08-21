/**
 * The takeover receipt (03 §2).
 *
 * Both capture channels take a download away from the browser: `takeover.ts` cancels the
 * `DownloadItem` and then **erases** it, and `intercept.ts` cancels the response before one
 * exists at all. Either way the browser's own download UI shows a flicker, or nothing, and
 * then holds no record — so from where the user is sitting, a click on a link produced
 * nothing. The honest reading of nothing is *it failed*, and the honest response to that is
 * to click again, which hands the same file over a second time.
 *
 * This is the sentence that closes that loop, and it is deliberately the smallest thing
 * that can: it names the file, it says where the file went, and it leaves.
 *
 * Four properties, in the order they were argued for:
 *
 * - **It goes somewhere.** The card is one button, and pressing it opens Vortex's popup —
 *   the panel the toolbar icon opens, where the file it just named appears in a list with
 *   a bar under it. A card that announces a file and then cannot be followed is a dead
 *   end, and the destination it points at is the one place the user was going to have to
 *   find anyway.
 * - **It cannot swallow the click that summoned it.** See {@link ARM}. This is what
 *   `pointer-events: none` used to buy outright, and it is bought here for the window
 *   where it actually matters instead.
 * - **It is not per download.** Five links clicked in five seconds are one card that
 *   counts, not five cards in a stack. A batch handover is exactly what people do with a
 *   download manager.
 * - **It leaves by itself.** No dismiss button, because a control implies a decision and
 *   there is none here — and it waits under the cursor, because a card someone is reaching
 *   for must not vanish out from under them.
 * - **It is inside a closed shadow root**, for the same reason the overlay is: page CSS
 *   cannot reach it, page scripts cannot read it, and a page that could enumerate it could
 *   tell that Vortex is installed.
 */

import css from "./style.css?inline";
import tokens from "@vortex/tokens/tokens.css?inline";

/** How long the card stays up before it starts leaving. Matches the overlay's own intro. */
const DWELL = 4000;
/** The fade. Long enough to read as leaving, short enough not to be in the way. */
const FADE = 200;

/**
 * How long the card refuses clicks after it appears.
 *
 * It arrives unasked, in the corner of a page someone is using, a few hundred milliseconds
 * after they clicked a link — and a control that materialises under a moving cursor can
 * take a click that was meant for the page behind it. That is a worse bug than the silence
 * this card was built to fix, because it is one the user cannot see happen.
 *
 * So the card is inert for longer than it takes to fade in. Nobody aims at something that
 * was not there half a second ago; a click inside this window is a click that was already
 * on its way somewhere else, and it should land there.
 */
const ARM = 500;

export class Receipt {
  private readonly host: HTMLElement;
  private readonly root: ShadowRoot;
  private readonly card: HTMLElement;
  private readonly name: HTMLElement;
  private readonly what: HTMLElement;
  /** How many handovers this card is currently speaking for. Reset when it leaves. */
  private count = 0;
  private arming: ReturnType<typeof setTimeout> | null = null;
  private leaving: ReturnType<typeof setTimeout> | null = null;
  private removing: ReturnType<typeof setTimeout> | null = null;

  /**
   * @param open What a press asks for. The card cannot open the popup itself — that is an
   * extension API, and a content script is not the extension — so this is a message to the
   * background and back out through `action.openPopup` (`src/toolbar.ts`).
   */
  constructor(private readonly open: () => void) {
    this.host = document.createElement("vortex-receipt");
    this.root = this.host.attachShadow({ mode: "closed" });
    // `:host` loses to any page rule that names the element, and these three are what keep
    // the card off the page's own layout entirely. Not left to the cascade.
    //
    // `pointer-events: none` stays on the *host* whatever the card is doing. The host is a
    // fixed, sizeless box that the card hangs off; nothing in it should ever be hit, and
    // the card re-enables itself from the inside once it is armed — a descendant with
    // `pointer-events: auto` is a target even when its ancestor is not.
    for (const [property, value] of [
      ["display", "block"],
      ["position", "fixed"],
      ["pointer-events", "none"],
    ] as const) {
      this.host.style.setProperty(property, value, "important");
    }

    const style = document.createElement("style");
    style.textContent = `${tokens}\n${css}`;

    this.card = element("button", "card");
    // Out of the page's tab order on purpose. This appears unannounced and is gone in four
    // seconds; a control that inserts itself into someone's tabbing and then disappears
    // mid-sequence is a worse citizen than one that cannot be tabbed to. The durable,
    // keyboard-reachable route to the same panel is the toolbar button, which is what this
    // card is a shortcut for rather than a replacement of.
    this.card.tabIndex = -1;
    this.card.setAttribute("type", "button");
    this.card.addEventListener("click", () => {
      // `pointer-events` is what stops the click reaching here at all; this is what makes
      // the guard a rule of the component rather than a line in a stylesheet, and it is
      // the only form of it a test can actually press against — a dispatched event does no
      // hit-testing. See {@link ARM}.
      if (this.card.getAttribute("data-live") !== "true") return;
      this.open();
      // The panel it asked for is now the thing to look at. A card left behind over the
      // page is just the announcement of something that has already happened.
      this.leave();
    });
    // A card that fades out from under a cursor reaching for it is a control that punishes
    // being used. Hovering holds it; leaving starts the clock over rather than resuming it,
    // because the four seconds are "long enough to read", not a budget being spent.
    this.card.addEventListener("mouseenter", () => this.hold());
    this.card.addEventListener("mouseleave", () => this.countdown());

    this.what = element("div", "what");
    this.name = element("div", "name");
    const copy = element("div", "copy");
    copy.append(this.what, this.name);
    const arrow = element("span", "arrow");
    arrow.textContent = "↓";
    // The one mark that says this is a way through rather than a notice. Without it the
    // card is only discoverable as a control by hovering it, which nobody does to something
    // they have been told is about to leave.
    const go = element("span", "go");
    go.textContent = "›";
    this.card.append(arrow, copy, go);

    this.root.append(style, this.card);
  }

  /**
   * Says that one more download has moved, and restarts the clock.
   *
   * Everything is re-stated on every call rather than diffed, because the second call is
   * what turns "Downloading in Vortex" into "2 downloads in Vortex" and both lines change
   * together.
   */
  show(filename: string): void {
    this.count += 1;
    this.what.textContent =
      this.count === 1 ? "Downloading in Vortex" : `${this.count} downloads in Vortex`;
    // `textContent`, never `innerHTML`. This name came from a `Content-Disposition` header
    // on a server nobody vouches for, and a receipt is not the place to start trusting one.
    this.name.textContent = filename;

    // Fullscreen paints only the fullscreen element's subtree, so a card parented to the
    // document would be invisible under a full-screen player — which is a page someone can
    // absolutely start a download from.
    const home = document.fullscreenElement ?? document.documentElement;
    if (this.host.parentNode !== home) home.append(this.host);

    // `clear` disarms, so every call re-arms — including one that lands on a card already
    // up and clickable. The second handover came from a second click, and it deserves the
    // same guard as the first.
    this.clear();
    this.arming = setTimeout(() => this.card.setAttribute("data-live", "true"), ARM);
    // Two frames, not one. The element has to be in the document and have had its initial
    // styles resolved before the transition's start value counts as a start value; a single
    // frame is enough on some engines and lands mid-recalc on others.
    requestAnimationFrame(() => {
      requestAnimationFrame(() => this.card.setAttribute("data-show", "true"));
    });
    this.countdown();
  }

  destroy(): void {
    this.clear();
    this.count = 0;
    this.host.remove();
  }

  /** Test seam. The root is closed on purpose, and a test is the page. */
  __cardForTests(): HTMLElement {
    return this.card;
  }

  /** Starts, or restarts, the wait before the card leaves. */
  private countdown(): void {
    if (this.removing) return;
    this.hold();
    this.leaving = setTimeout(() => this.leave(), DWELL);
  }

  /** Stops the card leaving, without committing to when it will. */
  private hold(): void {
    if (this.leaving) clearTimeout(this.leaving);
    this.leaving = null;
  }

  private leave(): void {
    if (this.removing) return;
    this.hold();
    this.card.setAttribute("data-show", "false");
    // Inert for the fade, so the last thing it does is not catch a click on its way out —
    // and disarmed as well as inert, or a pending `ARM` could arm a card that is leaving.
    this.disarm();
    this.removing = setTimeout(() => {
      // Only now, so a card that is asked to speak again mid-fade is the same card coming
      // back rather than a new one sliding in behind the old one's removal.
      this.count = 0;
      this.host.remove();
    }, FADE);
  }

  /** Makes the card inert, and cancels any pending promise to un-inert it. */
  private disarm(): void {
    if (this.arming) clearTimeout(this.arming);
    this.arming = null;
    this.card.setAttribute("data-live", "false");
  }

  private clear(): void {
    if (this.removing) clearTimeout(this.removing);
    this.removing = null;
    this.disarm();
    this.hold();
  }
}

function element(tag: "div" | "span" | "button", className: string): HTMLElement {
  const node = document.createElement(tag);
  node.className = className;
  return node;
}
