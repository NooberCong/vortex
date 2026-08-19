/**
 * The wire to `vortexd`, through the native messaging host.
 *
 * ```text
 * this module ──stdio, JSON──▶ vortex-host ──named pipe, MessagePack──▶ vortexd
 * ```
 *
 * One rule governs everything here, and it is the rule the whole takeover path is built
 * on (03 §2): **if the daemon is not reachable, capture goes fully passive.** A download
 * manager that eats the user's download because its own service crashed is worse than no
 * download manager at all. So reachability is never a cached flag — it is a `Ping` that
 * has to come back, right now, before anything is cancelled.
 *
 * The MV3 service worker is evicted after ~30 s idle, taking the port with it. Every entry
 * point here therefore treats the connection as absent and re-establishes it; nothing
 * assumes a port survived the last wake.
 */

import type { Browser } from "wxt/browser";
import { browser } from "wxt/browser";

import type { Command, Event } from "@vortex/proto";
import { PROTOCOL_VERSION } from "@vortex/proto";
import { CLIENT_NAME, HOST_NAME } from "./build";

/** How long a round-trip may take before the daemon counts as unreachable. */
const PING_TIMEOUT = 1500;
/** How long to wait for an answer that involves the network, like a probe. */
export const REPLY_TIMEOUT = 12_000;

type Listener = (event: Event) => void;

let port: Browser.runtime.Port | null = null;
const listeners = new Set<Listener>();
/**
 * Requests still waiting for a reply.
 *
 * A dead port never answers, and waiting out the timeout for an answer that provably
 * cannot come is not caution — it is a stall. Capture is a decision made in front of the
 * user's download, and on Firefox in front of a suspended response.
 */
const pending = new Set<(event: Event | null) => void>();

/** Fired for every event from the daemon, including ones nobody is waiting on. */
export function onEvent(listener: Listener): void {
  listeners.add(listener);
}

/**
 * The live port, connecting if there isn't one.
 *
 * `connectNative` throws only when the host manifest is missing; a host that starts and
 * then fails to reach the daemon reports it by closing the port, which arrives as
 * `onDisconnect` rather than an exception. Both mean the same thing to us.
 */
function connect(): Browser.runtime.Port | null {
  if (port) return port;
  try {
    const fresh = browser.runtime.connectNative(HOST_NAME);
    fresh.onMessage.addListener((message) => {
      const event = message as Event;
      for (const listener of listeners) listener(event);
    });
    fresh.onDisconnect.addListener(() => {
      // `lastError` must be read or the platform logs it as unchecked.
      void browser.runtime.lastError;
      if (port !== fresh) return;
      port = null;
      // The host manifest exists — `connectNative` did not throw — but the daemon behind
      // it is not there. That is an answer, and it arrives now rather than at the timeout.
      for (const waiter of [...pending]) waiter(null);
    });
    port = fresh;
    // Introduce ourselves so a version mismatch is reported once, on connect, rather than
    // as a puzzling failure on the first real command.
    fresh.postMessage({
      cmd: "hello",
      client: CLIENT_NAME,
      protocol: PROTOCOL_VERSION,
    } satisfies Command);
    return fresh;
  } catch {
    port = null;
    return null;
  }
}

/** Sends a command. Returns `false` if there was no port to send it down. */
export function send(command: Command): boolean {
  const live = connect();
  if (!live) return false;
  try {
    live.postMessage(command);
    return true;
  } catch {
    // The port died between `connect` and `postMessage`.
    port = null;
    return false;
  }
}

/**
 * Sends a command and waits for the first event that `matches`.
 *
 * The protocol has no request ids: a `Probed` is matched by its URL and a `MediaFound` by
 * its tab, but an `Error` carries only a sentence. So concurrent requests can in principle
 * take each other's error. That is tolerable *here specifically* because every caller
 * treats a failed reply the same way — leave the browser alone, let the download proceed —
 * so the worst case of a stolen error is one takeover that does not happen. Adding
 * correlation ids to the wire to fix a failure mode that fails safe would be paying in
 * protocol complexity for nothing.
 */
export function request(
  command: Command,
  matches: (event: Event) => boolean,
  timeout = REPLY_TIMEOUT,
): Promise<Event | null> {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (event: Event | null) => {
      if (settled) return;
      settled = true;
      listeners.delete(listener);
      pending.delete(finish);
      clearTimeout(timer);
      resolve(event);
    };
    const listener: Listener = (event) => {
      if (matches(event)) finish(event);
    };
    const timer = setTimeout(() => finish(null), timeout);

    listeners.add(listener);
    pending.add(finish);
    if (!send(command)) finish(null);
  });
}

/**
 * Is the daemon reachable **right now**?
 *
 * Deliberately not memoised. This is the gate in front of `downloads.cancel`, and a stale
 * `true` here is precisely the bug that loses a user's download.
 */
export async function reachable(): Promise<boolean> {
  const pong = await request({ cmd: "ping" }, (e) => e.event === "pong", PING_TIMEOUT);
  return pong !== null;
}

/**
 * Drops the port so the next call reconnects. Used by the liveness alarm.
 *
 * Subscriptions deliberately survive: `onEvent` listeners are the extension's standing
 * interest in renewals and ladders, registered once at startup, and clearing them here
 * would mean a single failed ping silently disabled both for the life of the worker.
 */
export function reset(): void {
  try {
    port?.disconnect();
  } catch {
    // Already gone, which is the state we were aiming for.
  }
  port = null;
}

/** Test seam: also forgets subscriptions, which the background never wants to do. */
export function __resetForTests(): void {
  reset();
  listeners.clear();
  pending.clear();
}
