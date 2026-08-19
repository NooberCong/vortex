import { beforeEach, describe, expect, it, vi } from "vitest";
import { fakeBrowser } from "wxt/testing";
import { browser } from "wxt/browser";

import * as capture from "@/src/capture";

type Fired = (details: Record<string, unknown>) => void;

/**
 * Captures a listener at registration instead of implementing the event.
 *
 * The fake browser implements none of `webRequest`, and the platform's whole contribution
 * to this path is to call a listener with an object — so the test does that part itself.
 */
function record(event: { addListener: (...args: never[]) => void }, into: (l: Fired) => void) {
  vi.spyOn(event, "addListener").mockImplementation(((listener: Fired) => into(listener)) as never);
}

/** Registers the observer and hands back the listener that starts an envelope. */
function observing(): Fired {
  let began: Fired = () => {};
  record(browser.webRequest.onBeforeRequest, (listener) => (began = listener));
  for (const event of [
    browser.webRequest.onSendHeaders,
    browser.webRequest.onHeadersReceived,
    browser.webRequest.onCompleted,
    browser.webRequest.onErrorOccurred,
  ]) {
    record(event, () => {});
  }
  record(browser.tabs.onRemoved, () => {});
  capture.observe(() => {});
  return began;
}

describe("the envelope the daemon has to be able to read", () => {
  beforeEach(() => {
    fakeBrowser.reset();
  });

  it("stamps a whole number of milliseconds, whatever the browser hands it", () => {
    // Chrome's `webRequest` timestamps carry sub-millisecond precision. `capturedAt` is a
    // `u64` on the wire, and `serde_json` will not read a float into an integer field — so
    // a fractional stamp does not make the envelope slightly wrong, it makes the entire
    // command undeserialisable. The native host then drops it, with a warning on a stderr
    // the browser throws away, and every probe and every takeover dies in total silence.
    // This is that bug, and it is the only reason this test exists.
    const began = observing();
    began({
      requestId: "42",
      url: "https://example.com/master.m3u8",
      method: "GET",
      type: "xmlhttprequest",
      tabId: 7,
      timeStamp: 1787152012345.678,
    });

    const envelope = capture.inFlightEnvelope("42");
    expect(envelope, "no envelope was started").toBeDefined();
    expect(Number.isInteger(envelope!.capturedAt)).toBe(true);
    expect(envelope!.capturedAt).toBe(1787152012345);
  });

  it("falls back to the wall clock when the browser states no time", () => {
    const began = observing();
    began({
      requestId: "43",
      url: "https://example.com/master.m3u8",
      method: "GET",
      type: "xmlhttprequest",
      tabId: 7,
    });

    const envelope = capture.inFlightEnvelope("43");
    expect(Number.isInteger(envelope!.capturedAt)).toBe(true);
    expect(envelope!.capturedAt).toBeGreaterThan(0);
  });
});
