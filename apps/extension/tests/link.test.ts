import { beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";

import * as host from "@/src/host";

/**
 * Telling the user which "not" they are looking at.
 *
 * The extension installs on its own, so a machine with no app on it is an ordinary state
 * rather than a broken one — and it looks identical to a crashed daemon from up here:
 * nothing answers either way. Reporting both as "not running" sends someone who has never
 * installed Vortex to look for it in their task manager, which is where this went before
 * the popup could say otherwise.
 *
 * The whole distinction rests on one signal, and these tests are about not over-reading
 * it: the platform saying there is no such native host. That is written by the app's own
 * installer, so its absence means the app was never installed. Everything else — a
 * timeout, a host that started and died, a refusal nobody recognises — is a `stopped`
 * daemon, because sending a user who already has Vortex off to download it again is the
 * more expensive mistake.
 */

/** A port that goes nowhere: the platform disconnects it, with `reason` in `lastError`. */
function refusedBy(reason: string | null): void {
  const listeners: Array<() => void> = [];
  const port = {
    postMessage() {
      // Chromium's answer to a host it cannot start arrives *after* the send, as a
      // disconnect. Firefox throws instead, which is the case below.
      queueMicrotask(() => {
        lastError(reason);
        for (const listener of listeners) listener();
      });
    },
    disconnect() {},
    onMessage: { addListener: () => {} },
    onDisconnect: { addListener: (l: () => void) => listeners.push(l) },
  };
  vi.spyOn(browser.runtime, "connectNative").mockReturnValue(port as never);
}

/** A port that connects, accepts everything and never says anything back. */
function silent(): void {
  const port = {
    postMessage() {},
    disconnect() {},
    onMessage: { addListener: () => {} },
    onDisconnect: { addListener: () => {} },
  };
  vi.spyOn(browser.runtime, "connectNative").mockReturnValue(port as never);
}

/** A host that answers `Ping`. */
function alive(): void {
  const listeners: Array<(event: unknown) => void> = [];
  const port = {
    postMessage() {
      queueMicrotask(() => listeners.forEach((l) => l({ event: "pong" })));
    },
    disconnect() {},
    onMessage: { addListener: (l: (event: unknown) => void) => listeners.push(l) },
    onDisconnect: { addListener: () => {} },
  };
  vi.spyOn(browser.runtime, "connectNative").mockReturnValue(port as never);
}

/** `lastError` is a getter the platform defines only while a callback is running. */
function lastError(message: string | null): void {
  Object.defineProperty(browser.runtime, "lastError", {
    value: message === null ? undefined : { message },
    configurable: true,
    writable: true,
  });
}

describe("whether the app is missing or merely stopped", () => {
  beforeEach(() => {
    host.__resetForTests();
    lastError(null);
  });

  it("says ready when the daemon answers", async () => {
    alive();
    expect(await host.status()).toBe("ready");
  });

  it("says missing when Chromium reports no such host", async () => {
    refusedBy("Specified native messaging host not found.");
    expect(await host.status()).toBe("missing");
  });

  it("says missing when the host is registered to some other extension", async () => {
    // The manifest exists but does not list this id, so from here it is the same fact:
    // there is nothing on this machine that will talk to us, and reinstalling is the fix.
    refusedBy("Access to the specified native messaging host is forbidden.");
    expect(await host.status()).toBe("missing");
  });

  it("says missing when Firefox throws instead of disconnecting", async () => {
    vi.spyOn(browser.runtime, "connectNative").mockImplementation(() => {
      throw new Error("No such native application io.vortex.host");
    });
    expect(await host.status()).toBe("missing");
  });

  it("says stopped when the host starts and dies", async () => {
    // The app is installed — something answered the platform — and `vortexd` is not
    // running behind it. "Go and download it again" would be wrong and insulting.
    refusedBy("Native host has exited.");
    expect(await host.status()).toBe("stopped");
  });

  it("says stopped when the refusal is one nobody recognises", async () => {
    refusedBy("Error when communicating with the native messaging host.");
    expect(await host.status()).toBe("stopped");
  });

  it("says stopped when nothing answers at all", async () => {
    // A wedged host: connected, silent, no disconnect and no reason. There is no evidence
    // of a missing install here, so none is claimed.
    vi.useFakeTimers();
    try {
      silent();
      const answer = host.status();
      await vi.advanceTimersByTimeAsync(2000);
      expect(await answer).toBe("stopped");
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops claiming the app is missing once it answers again", async () => {
    // The install ordering that would otherwise produce a permanent lie: extension first,
    // app second. Nothing re-runs when the app appears — the next `status` has to be the
    // thing that notices, and a remembered refusal from before the install must not
    // outlive the connection that disproves it.
    refusedBy("Specified native messaging host not found.");
    expect(await host.status()).toBe("missing");

    host.reset();
    lastError(null);
    alive();
    expect(await host.status()).toBe("ready");

    // And a later failure with no reason at all does not resurrect the old one.
    host.reset();
    vi.useFakeTimers();
    try {
      silent();
      const answer = host.status();
      await vi.advanceTimersByTimeAsync(2000);
      expect(await answer).toBe("stopped");
    } finally {
      vi.useRealTimers();
    }
  });
});
