import { bytes } from "@vortex/proto";
import type { Decision, JobState, Resolution } from "@vortex/proto";

/**
 * The words (05 §Copy).
 *
 * Two rules, and they are the whole file. An error says **what happened and how to fix
 * it** — never a status code, never an apology, never "an error occurred". And an action
 * keeps its name through the entire flow: *Download* → *Downloading* → *Downloaded*,
 * *Pause* → *Paused*. A button whose verb does not match the state it produces is the
 * cheapest way to make an interface feel like a form.
 *
 * The sentences for a *failure* are the engine's, not this file's: `EngineError::
 * user_message` already writes them, in the same voice, because the daemon is the only
 * thing that knows what actually happened. What is here is the other half — the buttons a
 * `Decision` deserves, which the daemon deliberately does not decide.
 */

export interface Choice {
  label: string;
  /** `null` is "Reopen page", which is a browser action rather than a resolution. */
  resolution: Resolution | null;
  /** The one the Enter key takes. Never a destructive one. */
  preferred?: boolean;
}

export interface Prompt {
  title: string;
  detail?: string;
  choices: Choice[];
}

/** What to show when a job is waiting on the user. */
export function prompt(decision: Decision): Prompt {
  switch (decision.kind) {
    case "fileChanged":
      return {
        title: "The file changed on the server.",
        detail: decision.detail,
        choices: [
          { label: "Start over", resolution: { kind: "startOver" } },
          { label: "Keep both", resolution: { kind: "keepBoth" }, preferred: true },
        ],
      };
    case "diskFull":
      return {
        title: `Not enough room on ${decision.drive}`,
        detail: `Needs ${bytes(decision.needed)} more.`,
        choices: [
          { label: "Change folder", resolution: null, preferred: true },
          { label: "Cancel", resolution: { kind: "cancel" } },
        ],
      };
    case "nameConflict":
      return {
        title: "There is already a file with that name.",
        detail: decision.path,
        choices: [
          { label: "Keep both", resolution: { kind: "keepBoth" }, preferred: true },
          { label: "Replace", resolution: { kind: "startOver" } },
        ],
      };
    case "permissionDenied":
      return {
        title: "Vortex can't write to that folder.",
        detail: decision.path,
        choices: [{ label: "Change folder", resolution: null, preferred: true }],
      };
    case "urlUnrecoverable":
      return {
        title: "The link expired.",
        // The one case where the fix is somewhere else entirely: the page mints the URL,
        // so the page is where the user has to go. Saying so is more useful than a retry
        // button that will fail again.
        detail: "Reopen the page to continue.",
        choices: [
          { label: "Reopen page", resolution: null, preferred: true },
          { label: "Cancel", resolution: { kind: "cancel" } },
        ],
      };
    case "integrityMismatch":
      return {
        title: "The file didn't match its checksum.",
        detail: decision.detail,
        choices: [
          { label: "Start over", resolution: { kind: "startOver" }, preferred: true },
          { label: "Cancel", resolution: { kind: "cancel" } },
        ],
      };
    case "muxFailed":
      return {
        title: "Combining the video and audio didn't finish.",
        // Worth saying: the expensive part is done and a retry is seconds, not another
        // download. A user who does not know that assumes they have lost the file.
        detail: `${decision.detail} The downloaded parts are still here.`,
        choices: [
          { label: "Try again", resolution: { kind: "retry" }, preferred: true },
          { label: "Cancel", resolution: { kind: "cancel" } },
        ],
      };
  }
}

/**
 * The word for a state, in the vocabulary the buttons use.
 *
 * `downloading` is deliberately absent: the segment map is already moving and already the
 * only coloured thing on the row, and a label saying so would be the second time the row
 * said one thing (05 §The one rule).
 */
export function label(state: JobState): string | null {
  switch (state.kind) {
    case "queued":
      return "Queued";
    case "probing":
      return "Checking";
    case "downloading":
      return null;
    case "stalled":
      return "Retrying";
    case "muxing":
      return "Combining";
    case "paused":
      return "Paused";
    case "needsDecision":
      return "Needs you";
    case "completed":
      return "Downloaded";
    case "failed":
      return "Failed";
  }
}

/** The sentence under the filename when a job has something to say for itself. */
export function note(state: JobState): string | null {
  switch (state.kind) {
    case "stalled":
      return state.reason;
    case "failed":
      return state.error;
    case "needsDecision":
      return prompt(state.decision).title;
    default:
      return null;
  }
}

/** The verb for the primary action on a row, which is also the state it produces. */
export function primaryAction(state: JobState): "pause" | "resume" | null {
  switch (state.kind) {
    case "downloading":
    case "probing":
    case "muxing":
      return "pause";
    case "paused":
    case "queued":
    case "stalled":
      return "resume";
    default:
      return null;
  }
}
