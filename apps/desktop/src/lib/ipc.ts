import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { Command, Event, JobId } from "@vortex/proto";

import { fixture } from "./fixture";

/**
 * The wire, from the window's side.
 *
 * There is exactly one channel in each direction: a `Command` goes out, an `Event` comes
 * back, and both types are generated from `crates/vortex-proto`. No request ids, no
 * promises that resolve with an answer — the daemon is a stream of state and the window
 * renders it. That is what makes a reconnect a repaint rather than a reconciliation.
 *
 * Subscriptions are the one thing the window does not control directly. It asks to
 * [`watch`] a job and the Rust side decides which of its two connections carries it; see
 * `src-tauri/src/link.rs` for why there are two.
 */
export interface Daemon {
  send(command: Command): Promise<void>;
  /** `null` collapses the expanded row and stops the 20 Hz frames it was costing. */
  watch(job: JobId | null): Promise<void>;
  connected(): Promise<boolean>;
  /** The tray's "pause all" / "resume all", which the window acts on because it owns the list. */
  onTray(handler: (intent: string) => void): Promise<() => void>;
  onEvent(handler: (event: Event) => void): Promise<() => void>;
  onLink(handler: (up: boolean) => void): Promise<() => void>;
  /** Live aggregate throughput, for the tray tooltip. */
  tooltip(text: string): Promise<void>;
}

/**
 * Whether this is the real window rather than `vite dev` in a browser tab.
 *
 * Anything that reaches for a native surface — window controls, the taskbar, the file
 * picker — asks first, because the alternative is a development build that throws on
 * mount and cannot be looked at.
 */
export const native = "__TAURI_INTERNALS__" in window;

const tauri: Daemon = {
  send: (command) => invoke("send", { command }),
  watch: (job) => invoke("watch", { job }),
  connected: () => invoke("connected"),
  tooltip: (text) => invoke("tooltip", { text }),
  onEvent: (handler) => listen<Event>("vortex://event", (e) => handler(e.payload)),
  onLink: (handler) =>
    listen<{ connected: boolean }>("vortex://link", (e) => handler(e.payload.connected)),
  onTray: (handler) => listen<string>("vortex://tray", (e) => handler(e.payload)),
};

/**
 * `vite dev` in a plain browser gets a scripted daemon instead of a real one.
 *
 * This is not a convenience. The quality floor in 05 is stated in numbers — 60 fps with
 * 40 jobs, under 400 ms to a painted list — and none of them can be measured against a
 * screen that needs forty real downloads to exist first. The fixture is how those numbers
 * get checked, so it lives beside the real transport rather than in a test directory.
 *
 * `import.meta.env.MODE` is a literal at build time, so the whole module folds away in a
 * production bundle. `tests/build.test.ts` asserts the built output contains none of it.
 */
export const daemon: Daemon = native ? tauri : fallback();

function fallback(): Daemon {
  // `MODE` rather than `DEV`, and the difference is load-bearing. Vite derives `DEV` from
  // `NODE_ENV` as well as from the mode, so `NODE_ENV=test vite build` — which is exactly
  // what a test runner spawns — leaves `DEV` true and ships the fixture. `MODE` is
  // "production" for every `vite build` regardless of the environment it was started in.
  // Written as a negation so the unit tests, which run under `MODE=test`, get the same
  // stand-in daemon the dev server gets. It folds to a literal either way, so in a real
  // build the call goes, the import goes, and the module goes with it.
  if (import.meta.env.MODE !== "production") return fixture();
  throw new Error("Vortex must run inside its own window.");
}
