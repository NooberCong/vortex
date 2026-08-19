import { describe, expect, it } from "vitest";

import { match } from "$lib/keys";

const press = (init: KeyboardEventInit & { target?: EventTarget }) => {
  const event = new KeyboardEvent("keydown", init);
  if (init.target) Object.defineProperty(event, "target", { value: init.target });
  return event;
};

const field = () => document.createElement("input");

describe("the keyboard", () => {
  it("binds the four shortcuts the spec names", () => {
    expect(match(press({ key: "n", ctrlKey: true }), false)).toBe("new");
    expect(match(press({ key: "f", ctrlKey: true }), false)).toBe("search");
    expect(match(press({ key: ",", ctrlKey: true }), false)).toBe("settings");
    expect(match(press({ key: " " }), false)).toBe("toggle");
  });

  it("uses Cmd on macOS and Ctrl everywhere else", () => {
    expect(match(press({ key: "n", metaKey: true }), true)).toBe("new");
    expect(match(press({ key: "n", ctrlKey: true }), true)).toBeNull();
    expect(match(press({ key: "n", metaKey: true }), false)).toBeNull();
  });

  it("leaves a bare key alone while the caret is in a field", () => {
    // Space is a space and Delete deletes a character. Getting this backwards makes an app
    // that pauses a download while you type a filename.
    const target = field();
    expect(match(press({ key: " ", target }), false)).toBeNull();
    expect(match(press({ key: "Delete", target }), false)).toBeNull();
    expect(match(press({ key: "ArrowDown", target }), false)).toBeNull();
  });

  it("still leaves a field on Escape", () => {
    expect(match(press({ key: "Escape", target: field() }), false)).toBe("close");
  });

  it("ignores a bare key that arrived with a modifier", () => {
    // `Ctrl Space` belongs to the input method, not to the download manager.
    expect(match(press({ key: " ", ctrlKey: true }), false)).toBeNull();
    expect(match(press({ key: "ArrowDown", altKey: true }), false)).toBeNull();
  });

  it("says nothing about keys it does not bind", () => {
    expect(match(press({ key: "q" }), false)).toBeNull();
    expect(match(press({ key: "F5" }), false)).toBeNull();
  });
});
