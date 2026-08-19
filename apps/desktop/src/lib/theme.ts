import { getCurrentWindow } from "@tauri-apps/api/window";

import type { Settings, Theme } from "@vortex/proto";

import { native } from "./ipc";
import { invalidate } from "./palette";

/**
 * Appearance, applied to the three places that have to agree.
 *
 * The stylesheet reads `data-theme` off the root element. The canvases read resolved
 * colours and have to be told the answer changed. And the native window has its own idea
 * of light or dark, which decides the Mica tint and the scrollbar the OS draws — leaving
 * that one out is why a dark app sometimes has a white flash on resize.
 *
 * The setting itself lives in the daemon like every other one, so this only ever applies
 * what it is given.
 */
export function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
  invalidate();
  // `null` hands the decision back to the OS, which is what "system" means.
  if (native) void getCurrentWindow().setTheme(theme === "system" ? null : theme);
}

/**
 * The reduced-motion preference, from either source.
 *
 * The OS setting is the one most users have; the app setting exists for the machine where
 * it is not exposed, or where someone wants a calm download manager and a lively
 * everything else. Either one is enough, and neither overrides the other.
 */
export function applyMotion(reduced: boolean): void {
  document.documentElement.dataset.motion = reduced ? "reduced" : "full";
}

/** Applies everything a `Settings` says about appearance. */
export function applyAppearance(settings: Settings): void {
  applyTheme(settings.theme);
  applyMotion(settings.reducedMotion);
}
