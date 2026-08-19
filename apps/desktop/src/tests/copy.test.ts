import { describe, expect, it } from "vitest";

import type { Decision, JobState } from "@vortex/proto";

import { label, note, primaryAction, prompt } from "$lib/copy";

const DECISIONS: Decision[] = [
  { kind: "fileChanged", detail: "The ETag changed." },
  { kind: "diskFull", needed: 2_100_000_000, drive: "D:" },
  { kind: "nameConflict", path: "D:\\Downloads\\report.pdf" },
  { kind: "permissionDenied", path: "C:\\Program Files" },
  { kind: "urlUnrecoverable", pageUrl: "https://example.com/watch" },
  { kind: "integrityMismatch", detail: "SHA-256 did not match." },
  { kind: "muxFailed", detail: "ffmpeg exited 1." },
];

describe("what a job asks the user", () => {
  it("always says what happened and offers a way out", () => {
    for (const decision of DECISIONS) {
      const asked = prompt(decision);
      expect(asked.title, decision.kind).not.toBe("");
      // Never a bare code, never an apology (05 §Copy).
      expect(asked.title).not.toMatch(/error|sorry|failed to|unexpected/i);
      expect(asked.choices.length, decision.kind).toBeGreaterThan(0);
    }
  });

  it("marks exactly one choice as the one Enter takes, and never a destructive one", () => {
    for (const decision of DECISIONS) {
      const preferred = prompt(decision).choices.filter((c) => c.preferred);
      expect(preferred, decision.kind).toHaveLength(1);
      expect(preferred[0]!.label).not.toMatch(/delete|remove|replace/i);
    }
  });

  it("writes the sentences the spec writes", () => {
    expect(prompt({ kind: "fileChanged", detail: "" }).title).toBe(
      "The file changed on the server.",
    );
    expect(prompt({ kind: "urlUnrecoverable", pageUrl: null })).toMatchObject({
      title: "The link expired.",
      detail: "Reopen the page to continue.",
    });
    // The size is rendered by the shared formatter, so `2.1 GB` and not `2100000000 bytes`.
    expect(prompt({ kind: "diskFull", needed: 2_100_000_000, drive: "D:" })).toMatchObject({
      title: "Not enough room on D:",
      detail: "Needs 2.1 GB more.",
    });
  });

  it("offers to change the folder rather than to retry when the folder is the problem", () => {
    for (const kind of ["diskFull", "permissionDenied"] as const) {
      const decision = DECISIONS.find((d) => d.kind === kind)!;
      expect(prompt(decision).choices[0]!.label).toBe("Change folder");
    }
  });
});

describe("the vocabulary", () => {
  it("gives a moving job no state label at all", () => {
    // The map is already moving and is already the only coloured thing on the row. Saying
    // it in words as well is the second time the row said one thing (05 §The one rule).
    expect(label({ kind: "downloading" })).toBeNull();
  });

  it("keeps an action's name through the whole flow", () => {
    // Download → Downloading → Downloaded; Pause → Paused. A button whose verb does not
    // match the state it produces is the cheapest way to feel like a form.
    expect(label({ kind: "paused" })).toBe("Paused");
    expect(label({ kind: "completed" })).toBe("Downloaded");
    expect(primaryAction({ kind: "downloading" })).toBe("pause");
    expect(primaryAction({ kind: "paused" })).toBe("resume");
    expect(primaryAction({ kind: "completed" })).toBeNull();
  });

  it("names every state it can be handed", () => {
    const states: JobState[] = [
      { kind: "queued" },
      { kind: "probing" },
      { kind: "downloading" },
      { kind: "stalled", reason: "x" },
      { kind: "muxing" },
      { kind: "paused" },
      { kind: "needsDecision", decision: DECISIONS[0]! },
      { kind: "completed" },
      { kind: "failed", error: "x" },
    ];
    for (const state of states) {
      const named = label(state);
      expect(named === null || named.length > 0, state.kind).toBe(true);
    }
  });

  it("repeats the daemon's own sentence rather than writing a second one", () => {
    // `EngineError::user_message` already writes these, in the same voice, and it is the
    // only thing that knows what actually happened.
    expect(note({ kind: "stalled", reason: "Lost connection. Retrying." })).toBe(
      "Lost connection. Retrying.",
    );
    expect(note({ kind: "failed", error: "The file is no longer on the server." })).toBe(
      "The file is no longer on the server.",
    );
    expect(note({ kind: "downloading" })).toBeNull();
  });
});
