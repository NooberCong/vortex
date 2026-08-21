import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Receipt } from "@/src/receipt";

/**
 * The takeover receipt.
 *
 * Both capture channels erase the browser's own record of a download before refetching it,
 * so the click that started it looks, from the page, like a click that did nothing — and
 * the response to nothing is to click again and get the file twice (03 §2). This card is
 * the sentence that stops that, and everything below is about it being the *smallest*
 * thing that can: it names the file, it opens the one panel that has more to say, and it
 * goes away.
 */

/** The card, out of a shadow root that is closed to everything except its own test seam. */
function card(receipt: Receipt): HTMLElement {
  return receipt.__cardForTests();
}

const shown = (receipt: Receipt) => card(receipt).getAttribute("data-show") === "true";
const live = (receipt: Receipt) => card(receipt).getAttribute("data-live") === "true";
const said = (receipt: Receipt) => ({
  what: card(receipt).querySelector(".what")?.textContent ?? "",
  name: card(receipt).querySelector(".name")?.textContent ?? "",
});

/** Two frames, which is what `show` waits for before it starts the transition. */
async function painted(): Promise<void> {
  await vi.advanceTimersByTimeAsync(50);
}

/** Painted, and then armed — the point from which a press is a press. */
async function ready(): Promise<void> {
  await vi.advanceTimersByTimeAsync(500);
}

function hover(receipt: Receipt, over: boolean): void {
  card(receipt).dispatchEvent(new Event(over ? "mouseenter" : "mouseleave"));
}

let receipt: Receipt;
let opened: number;

beforeEach(() => {
  vi.useFakeTimers();
  document.documentElement.replaceChildren(document.createElement("body"));
  opened = 0;
  receipt = new Receipt(() => {
    opened += 1;
  });
});

afterEach(() => {
  receipt.destroy();
  // Defined by the fullscreen test below and not otherwise present on happy-dom's
  // document, so it has to be put back or it leaks into whatever runs next.
  Object.defineProperty(document, "fullscreenElement", { value: undefined, configurable: true });
  vi.useRealTimers();
});

describe("what it says", () => {
  it("names the file, because 'a download started' is not news", async () => {
    receipt.show("ubuntu-24.04.2-live-server-amd64.iso");
    await painted();
    expect(said(receipt)).toEqual({
      what: "Downloading in Vortex",
      name: "ubuntu-24.04.2-live-server-amd64.iso",
    });
    expect(shown(receipt)).toBe(true);
  });

  it("counts rather than stacks, because people click five links at once", async () => {
    receipt.show("one.iso");
    await painted();
    receipt.show("two.iso");
    await painted();
    receipt.show("three.iso");
    await painted();

    expect(said(receipt)).toEqual({ what: "3 downloads in Vortex", name: "three.iso" });
    // One card, one host element, however many downloads it is speaking for.
    expect(document.querySelectorAll("vortex-receipt")).toHaveLength(1);
  });

  it("never puts a server's filename anywhere it could be markup", async () => {
    // This name came from a `Content-Disposition` header on a host nobody vouches for.
    receipt.show("<img src=x onerror=alert(1)>.zip");
    await painted();
    expect(card(receipt).querySelector("img")).toBeNull();
    expect(said(receipt).name).toBe("<img src=x onerror=alert(1)>.zip");
  });
});

describe("where it goes", () => {
  it("asks for the panel when pressed, and stops speaking once it has", async () => {
    receipt.show("one.iso");
    await ready();

    card(receipt).dispatchEvent(new Event("click"));
    expect(opened).toBe(1);
    // The panel is the thing to look at now. A card left over the page is the announcement
    // of something that has already happened.
    expect(shown(receipt)).toBe(false);
    await vi.advanceTimersByTimeAsync(200);
    expect(document.querySelector("vortex-receipt")).toBeNull();
  });

  it("is one button, so there is nothing in it to miss", async () => {
    receipt.show("one.iso");
    await painted();
    expect(card(receipt).tagName).toBe("BUTTON");
    // The affordance is static, not a hover state. Nobody hovers something they have been
    // told is about to leave, to find out whether it is a control.
    expect(card(receipt).querySelector(".go")?.textContent).toBe("›");
  });

  it("stays out of the page's tab order", async () => {
    // It appears unannounced and is gone in four seconds. A control that inserts itself
    // into someone's tabbing and then disappears mid-sequence is a worse citizen than one
    // that cannot be tabbed to; the keyboard route to the same panel is the toolbar button.
    receipt.show("one.iso");
    await painted();
    expect((card(receipt) as HTMLButtonElement).tabIndex).toBe(-1);
  });
});

describe("how it behaves", () => {
  it("refuses the click that summoned it", async () => {
    // The card arrives a few hundred milliseconds after a click on the page, in a corner
    // someone may be moving through. A control that materialises under a moving cursor can
    // take a click meant for the page behind it — which is a worse bug than the silence
    // this card fixes, because the user cannot see it happen.
    receipt.show("one.iso");
    await painted();
    expect(live(receipt)).toBe(false);
    card(receipt).dispatchEvent(new Event("click"));
    expect(opened).toBe(0);
    expect(shown(receipt), "and it is still there to be pressed properly").toBe(true);

    await ready();
    expect(live(receipt)).toBe(true);
    card(receipt).dispatchEvent(new Event("click"));
    expect(opened).toBe(1);
  });

  it("re-arms on a second handover, because that was a second click", async () => {
    receipt.show("one.iso");
    await ready();
    expect(live(receipt)).toBe(true);

    receipt.show("two.iso");
    await painted();
    expect(live(receipt)).toBe(false);
  });

  it("is never the thing a stray click lands on", async () => {
    // The host is a fixed, sizeless box the card hangs off, and it is inert whatever the
    // card is doing. `!important` because `:host` loses to any page rule naming the element.
    receipt.show("one.iso");
    await ready();
    const host = document.querySelector("vortex-receipt") as HTMLElement;
    expect(host.style.pointerEvents).toBe("none");
    expect(host.style.getPropertyPriority("pointer-events")).toBe("important");
  });

  it("waits under the cursor rather than fading out of reach", async () => {
    receipt.show("one.iso");
    await ready();
    hover(receipt, true);

    // Well past the dwell. A control that punished being reached for would be worse than
    // one that could not be reached at all.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(shown(receipt)).toBe(true);
    expect(live(receipt)).toBe(true);

    // And the clock starts over rather than resuming: four seconds is "long enough to
    // read", not a budget that was being spent.
    hover(receipt, false);
    await vi.advanceTimersByTimeAsync(3900);
    expect(shown(receipt)).toBe(true);
    await vi.advanceTimersByTimeAsync(300);
    expect(document.querySelector("vortex-receipt")).toBeNull();
  });

  it("leaves by itself, and takes its element with it", async () => {
    receipt.show("one.iso");
    await painted();
    expect(document.querySelector("vortex-receipt")).not.toBeNull();

    await vi.advanceTimersByTimeAsync(4000);
    expect(shown(receipt), "it starts fading at the dwell").toBe(false);
    expect(document.querySelector("vortex-receipt"), "and is still there while it does").not.toBeNull();
    // Inert on the way out, so the last thing it does is not catch a click.
    expect(live(receipt)).toBe(false);

    await vi.advanceTimersByTimeAsync(200);
    expect(document.querySelector("vortex-receipt")).toBeNull();
  });

  it("comes back as the same card when a download lands mid-fade", async () => {
    receipt.show("one.iso");
    await painted();
    await vi.advanceTimersByTimeAsync(4000);

    // Not a second card sliding in behind the first one's removal.
    receipt.show("two.iso");
    await painted();
    expect(shown(receipt)).toBe(true);
    expect(said(receipt).what, "still the same card, so still counting").toBe(
      "2 downloads in Vortex",
    );

    await vi.advanceTimersByTimeAsync(4200);
    expect(document.querySelector("vortex-receipt")).toBeNull();
  });

  it("starts counting again once it has actually gone", async () => {
    receipt.show("one.iso");
    await painted();
    await vi.advanceTimersByTimeAsync(4200);

    receipt.show("two.iso");
    await painted();
    expect(said(receipt).what).toBe("Downloading in Vortex");
  });

  it("draws inside the fullscreen element, which is the only thing painted there", async () => {
    const player = document.createElement("div");
    document.body.append(player);
    // happy-dom has no `fullscreenElement`, so it is defined rather than spied on.
    Object.defineProperty(document, "fullscreenElement", { value: player, configurable: true });

    receipt.show("one.iso");
    await painted();
    expect(player.querySelector("vortex-receipt")).not.toBeNull();
  });

  it("is gone for good once destroyed", async () => {
    receipt.show("one.iso");
    await painted();
    receipt.destroy();
    expect(document.querySelector("vortex-receipt")).toBeNull();

    // An extension reload leaves this script in a page whose runtime is gone, and a timer
    // that fires after it would be an "Extension context invalidated" per tab.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(document.querySelector("vortex-receipt")).toBeNull();
  });
});
