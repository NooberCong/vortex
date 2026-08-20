import { ProgressBarStatus } from "@tauri-apps/api/window";
import { describe, expect, it } from "vitest";

import { taskbar } from "$lib/aggregate.svelte";

/**
 * What the taskbar button says while the app is working.
 *
 * The rule this pins is the one that is easy to get wrong by omission: a queue that is
 * working but cannot be measured must still animate. Sending "no progress" there is the
 * difference between an app that looks busy and an app that looks closed, and it is the
 * common case for media — an extractor that will not declare a length, a chunked stream, a
 * mux that has no bytes to count.
 */
describe("the taskbar button", () => {
  it("is plain when nothing is working", () => {
    expect(taskbar(0, null)).toEqual({ status: ProgressBarStatus.None });
  });

  it("stays plain when nothing is working, whatever the last measurement was", () => {
    // A finished queue still has totals to divide; none of them are moving.
    expect(taskbar(0, 0.42)).toEqual({ status: ProgressBarStatus.None });
  });

  it("fills to the share of the queue that is done", () => {
    expect(taskbar(2, 0.42)).toEqual({ status: ProgressBarStatus.Normal, progress: 42 });
  });

  it("animates when something is working but nothing can be measured", () => {
    expect(taskbar(1, null)).toEqual({ status: ProgressBarStatus.Indeterminate });
  });
});
