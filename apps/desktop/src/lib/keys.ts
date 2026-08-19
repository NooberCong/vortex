/**
 * The keyboard (05 §Platform polish).
 *
 * A pure function from an event to an intent, so the bindings can be tested without a
 * window and the component that dispatches them stays a dispatcher. The whole map is
 * visible in one screen, which is the point — a shortcut nobody can find in the source is
 * a shortcut nobody documented either.
 *
 * The typing rule is the one that matters. While the caret is in a field, Space is a
 * space and Delete deletes a character; only Escape still means "leave". Getting that
 * backwards makes an app that pauses a download when you type a filename.
 */

export type Intent =
  | "new"
  | "search"
  | "settings"
  | "toggle"
  | "remove"
  | "expand"
  | "up"
  | "down"
  | "close";

/** Modifier-based bindings use Cmd on macOS and Ctrl everywhere else. */
export function match(event: KeyboardEvent, mac = navigator.userAgent.includes("Mac")): Intent | null {
  const accel = mac ? event.metaKey : event.ctrlKey;
  const key = event.key;

  if (accel && !event.shiftKey && !event.altKey) {
    if (key === "n" || key === "N") return "new";
    if (key === "f" || key === "F") return "search";
    if (key === ",") return "settings";
  }

  if (key === "Escape") return "close";

  // Everything below is a bare key, and a bare key belongs to whatever is being typed in.
  if (typing(event.target)) return null;
  if (event.ctrlKey || event.metaKey || event.altKey) return null;

  switch (key) {
    case " ":
      return "toggle";
    case "Delete":
    case "Backspace":
      return "remove";
    case "Enter":
      return "expand";
    case "ArrowUp":
      return "up";
    case "ArrowDown":
      return "down";
    default:
      return null;
  }
}

function typing(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return (
    tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA" || target.isContentEditable
  );
}
