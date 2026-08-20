import { browser } from "wxt/browser";

import type { MediaCandidate, MediaTrack, MediaVariant } from "@vortex/proto";
import { MEDIA_CAPTURE, RELEASES } from "@/src/build";
import type { FromPage, PopupState } from "@/src/messages";
import { usableCandidates } from "@/src/overlay/anchor";
import { codec, duration, estimate, quality } from "@/src/overlay/format";
import { carry, selectionOf, type State } from "@/src/overlay/select";
import "@vortex/tokens/tokens.css";
import "./style.css";

/**
 * The toolbar popup.
 *
 * It exists because the overlay could not be the only way in. The overlay's "Not on this
 * site" switch removes the overlay, which made it a one-way door: from that moment every
 * capture path returned silently for that origin, and the only control that could have
 * undone it was the thing that had just deleted itself. Nothing on the page said why, and
 * the honest answer to "why is there no download button" lived in a JSON file the user was
 * never told about. That is the bug this window closes (03 §The overlay).
 *
 * So it answers three questions, in the order someone actually asks them:
 *
 * 1. **Is Vortex working at all?** The daemon light, first, because nothing below it can
 *    do anything while the answer is no — and if the app was never installed, the way to
 *    get it, because "not running" is a useless thing to tell someone who has nothing to
 *    run (`src/host.ts`).
 * 2. **Is it switched on here?** The site switch, which is the only way back on.
 * 3. **What did it find?** Every confirmed stream on this tab, with its ladder.
 *
 * It renders from one message and re-renders wholesale. A popup is open for a few seconds
 * and is destroyed on blur, so there is no live state worth keeping and anything clever
 * about incremental updates would be complexity spent on a window nobody watches.
 */

void main();

async function main(): Promise<void> {
  const root = document.getElementById("root");
  if (!root) return;

  const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
  const tabId = tab?.id;
  if (tabId === undefined) {
    root.append(empty("No page here", "Open this over a tab with a video on it."));
    return;
  }

  const state = (await send({ kind: "popupState", tabId })) as PopupState | undefined;
  if (!state) {
    // No answer from the background, which under MV3 means the worker could not be woken.
    root.append(empty("Vortex is not responding", "Try reloading the extension."));
    return;
  }
  draw(root, state, tabId);
}

function draw(root: HTMLElement, state: PopupState, tabId: number): void {
  const redraw = () => {
    root.replaceChildren();
    draw(root, state, tabId);
  };
  root.append(head(state));
  // Above the site switch, because a switch for a program that is not on the machine is
  // not the thing to read first.
  if (state.daemon === "missing") root.append(install());
  root.append(site(state, redraw));
  if (MEDIA_CAPTURE) root.append(streams(state, tabId));
}

// -- Header ------------------------------------------------------------------

function head(state: PopupState): HTMLElement {
  const bar = el("div", "head");

  const mark = el("div", "wordmark");
  const logo = document.createElement("img");
  logo.src = browser.runtime.getURL("/icon/128.png");
  logo.alt = "";
  mark.append(logo, text("span", "", "Vortex"));

  const status = text("div", "status", LIGHT[state.daemon].label);
  status.dataset.up = String(state.daemon === "ready");
  status.title = LIGHT[state.daemon].detail;

  bar.append(mark, status);
  return bar;
}

/** What the light says, in the three states the connection actually has. */
const LIGHT: Record<PopupState["daemon"], { label: string; detail: string }> = {
  ready: { label: "Ready", detail: "The Vortex daemon is answering." },
  stopped: {
    label: "Not running",
    detail: "Start Vortex. Nothing can be downloaded until it is running.",
  },
  missing: {
    label: "Not installed",
    detail: "The Vortex app is not on this machine. The extension cannot download without it.",
  },
};

// -- Getting the app ---------------------------------------------------------

/**
 * The other half of Vortex, offered when the machine does not have it.
 *
 * The extension can be installed on its own — from a store listing, from a zip — and on
 * its own it does nothing at all: it watches downloads and hands them somewhere, and with
 * nowhere to hand them it is an icon that shrugs. Everything else on this panel would have
 * gone on quietly reporting "not running" at a program that was never installed, which is
 * a sentence that sends people to look at their task manager (`src/host.ts`).
 *
 * Only ever shown on `missing`, which is a registered-host manifest that is not there
 * rather than a ping that went unanswered. A stopped daemon gets the light and no button.
 */
function install(): HTMLElement {
  const section = document.createElement("section");
  section.className = "install";
  section.append(
    text("div", "what", "Vortex is not installed"),
    text(
      "div",
      "why",
      "This extension is the capture half: it spots downloads and hands them over. " +
        "The app is what fetches them, and it has to be installed separately.",
    ),
  );

  const get = document.createElement("button");
  get.className = "get";
  get.type = "button";
  get.textContent = "Get Vortex";
  get.addEventListener("click", () => {
    // Closed only once the tab exists, so a popup that dies mid-call cannot swallow it.
    void browser.tabs.create({ url: RELEASES }).then(() => window.close());
  });
  section.append(get);
  return section;
}

// -- The site switch ---------------------------------------------------------

/**
 * The control the overlay could not offer.
 *
 * Checked means "capture is on here", which is the direction that reads correctly at a
 * glance: a switch someone turned off is a switch they can see is off. The label says what
 * it does to the *page* rather than naming the setting, because what the user remembers is
 * the button that stopped appearing, not the word "opt-out".
 */
function site(state: PopupState, redraw: () => void): HTMLElement {
  const section = document.createElement("section");
  section.append(text("div", "eyebrow", "This site"));

  const origin = state.origin;
  if (!origin) {
    section.append(
      text("div", "origin", "No site"),
      text("div", "note", "This page has no address Vortex can hold a setting against."),
    );
    return section;
  }

  section.append(text("div", "origin", hostOf(origin)));

  const row = el("label", "switch");
  const copy = el("span", "copy");
  copy.append(text("span", "what", "Offer downloads on this site"), text("span", "why", why(state)));

  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = !state.optedOut;
  // The list this writes to belongs to the daemon, and it is the daemon that reads it back
  // for every capture decision. With nothing on the other end the change would be accepted
  // here, applied nowhere, and gone by the next time the panel was opened — a switch that
  // lies about what it did is worse than one that says it cannot.
  input.disabled = state.daemon !== "ready";
  input.addEventListener("change", () => {
    state.optedOut = !input.checked;
    void send({ kind: "siteCapture", origin, on: input.checked });
    redraw();
  });

  row.append(copy, input);
  section.append(row);

  if (state.captureOff && state.daemon === "ready") {
    // A site switch that is on and still does nothing is worse than one that is off, so
    // the global setting has to say so where the per-site one is being read.
    section.append(
      text("div", "note", "Capture is switched off for every site in Vortex settings."),
    );
  }
  return section;
}

/** What the switch is currently doing — including "nothing", when that is the truth. */
function why(state: PopupState): string {
  if (state.daemon === "missing") return "Vortex is not installed, so nothing is captured here.";
  if (state.daemon === "stopped") return "Vortex is not running, so this cannot be changed.";
  // Worded for what the switch *does* — capture — rather than for what the full build
  // draws on top of it. The overlay does not exist in the store build, and a line
  // promising a button on a video would describe a feature that package does not have
  // (`src/build.ts`). Where the download goes is true of both.
  return state.optedOut
    ? "Off. Downloads here are left to the browser."
    : "On. Downloads here are handed to Vortex.";
}

// -- Captured streams --------------------------------------------------------

function streams(state: PopupState, tabId: number): HTMLElement {
  const section = document.createElement("section");
  const found = usableCandidates(state.candidates);

  if (found.length === 0) {
    section.append(text("div", "eyebrow", "Captured"), nothing(state));
    return section;
  }

  section.append(
    text("div", "eyebrow", found.length === 1 ? "1 stream" : found.length + " streams"),
  );
  for (const candidate of found) section.append(card(candidate, state, tabId));
  return section;
}

/** Why the list is empty, which is never the same reason twice. */
function nothing(state: PopupState): HTMLElement {
  if (state.daemon === "missing") {
    return empty("Vortex is not installed", "Install the app above, then reload the page.");
  }
  if (state.daemon === "stopped") {
    return empty("Vortex is not running", "Start the app, then reload the page.");
  }
  if (state.optedOut) {
    return empty("Switched off here", "Turn the switch above back on, then reload the page.");
  }
  if (state.captureOff) {
    return empty("Capture is off", "Switch it on in Vortex settings, then reload the page.");
  }
  return empty(
    "Nothing captured yet",
    "Play the video and it will appear here. Some players only reveal the stream once they start.",
  );
}

function card(candidate: MediaCandidate, page: PopupState, tabId: number): HTMLElement {
  const box = el("div", "card");
  // The same starting point the overlay uses, from the same function: the rung the daemon
  // derived from the saved preference, never the largest one.
  const state = carry(null, candidate, null);

  const meta = [kindOf(candidate.kind), duration(candidate.durationSecs)].filter(Boolean);
  box.append(
    text("div", "title", candidate.title || page.pageTitle || "Video"),
    text("div", "meta", meta.join(" · ")),
  );

  const ladder = el("div", "ladder");
  candidate.variants.forEach((variant: MediaVariant, index: number) => {
    ladder.append(
      rung(
        quality(variant),
        variant.codecLabel,
        variant.estimatedBytes,
        index === state.variant,
        () => take(state, index, candidate, page, tabId),
      ),
    );
  });
  // Audio-only is the same job with the video track left out, and a real thing to want
  // from a lecture or a podcast.
  const track: MediaTrack | undefined = candidate.audio[0];
  if (track && candidate.variants.length > 0) {
    ladder.append(
      rung("Audio only", track.codecLabel, track.estimatedBytes, false, () =>
        take(state, -1, candidate, page, tabId),
      ),
    );
  }
  box.append(ladder);

  if (candidate.subtitles.length > 0) box.append(subtitles(state, candidate));
  return box;
}

/** A rung is the button. Choosing a resolution *is* the download, as in the overlay. */
function rung(
  name: string,
  codecLabel: string | null | undefined,
  size: number | null | undefined,
  here: boolean,
  chosen: () => void,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.className = "rung";
  button.type = "button";
  button.dataset.here = String(here);
  button.append(
    text("span", "name", name),
    text("span", "codec", codec(codecLabel)),
    text("span", "size", estimate(size)),
    text("span", "arrow", "↓"),
  );
  button.addEventListener("click", () => {
    chosen();
    // Said in place, on the thing that was pressed. A popup that closed itself here would
    // take the rest of the ladder with it, and a second rung is a normal thing to want.
    button.classList.add("queued");
    button.replaceChildren(text("span", "name", "Added to Vortex"));
  });
  return button;
}

function subtitles(state: State, candidate: MediaCandidate): HTMLElement {
  const row = el("label", "subs");
  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = state.subtitles;
  input.addEventListener("change", () => {
    state.subtitles = input.checked;
  });
  const languages = candidate.subtitles
    .map((track: MediaTrack) => track.label || track.language || "Unknown")
    .join(", ");
  row.append(input, text("span", "", "Subtitles"), text("span", "languages", languages));
  return row;
}

/** Hands the chosen rung to the daemon. */
function take(
  state: State,
  variant: number,
  candidate: MediaCandidate,
  page: PopupState,
  tabId: number,
): void {
  state.variant = variant;
  void send({
    kind: "download",
    selection: selectionOf(state),
    pageTitle: candidate.title || page.pageTitle,
    // A popup is in no tab, so it has to say which one it is talking about.
    tabId,
  });
}

// -- Small pieces ------------------------------------------------------------

function empty(heading: string, detail: string): HTMLElement {
  const box = el("div", "empty");
  const strong = document.createElement("strong");
  strong.textContent = heading;
  box.append(strong, document.createTextNode(detail));
  return box;
}

function kindOf(kind: MediaCandidate["kind"]): string {
  return kind === "Hls" ? "HLS" : kind === "Dash" ? "DASH" : "Direct";
}

/** A heading wants the host, not the scheme. */
function hostOf(origin: string): string {
  try {
    return new URL(origin).host;
  } catch {
    return origin;
  }
}

function send(message: FromPage): Promise<unknown> {
  try {
    return browser.runtime.sendMessage(message).catch(() => undefined);
  } catch {
    return Promise.resolve(undefined);
  }
}

// -- DOM sugar ---------------------------------------------------------------

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  node.className = className;
  return node;
}

function text(tag: "span" | "div", className: string, content: string): HTMLElement {
  const node = document.createElement(tag);
  if (className) node.className = className;
  // `textContent`, never `innerHTML`: every title here came from a page-controlled
  // manifest, and a popup is not the place to start trusting one.
  node.textContent = content;
  return node;
}
