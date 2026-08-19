import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";

import type { Command, JobId, JobSpec, Resolution, Settings } from "@vortex/proto";

import { daemon } from "./ipc";
import { queue } from "./store.svelte";

/**
 * Everything the window can ask for.
 *
 * Nothing here changes what is on screen. A command goes to the daemon, the daemon decides,
 * and the resulting `Event` updates the model — so a paused row is paused because the
 * daemon said so, never because a button was pressed. That is what stops this window and a
 * second one, or the CLI, from disagreeing.
 *
 * The cost is a round trip through a pipe on the same machine before a row visibly
 * changes. In exchange, "it says paused but it is still downloading" is not a state this
 * app can reach.
 */
async function send(command: Command): Promise<void> {
  try {
    await daemon.send(command);
  } catch (e) {
    // The Rust side answers with a sentence, not a code. Anything else is a bug here.
    queue.problem = typeof e === "string" ? e : "Vortex isn't running.";
  }
}

export const pause = (job: JobId) => send({ cmd: "pause", job });
export const resume = (job: JobId) => send({ cmd: "resume", job });
export const cancel = (job: JobId) => send({ cmd: "cancel", job });
export const remove = (job: JobId, deleteFile = false) =>
  send({ cmd: "remove", job, deleteFile });
export const decide = (job: JobId, resolution: Resolution) =>
  send({ cmd: "decide", job, resolution });
export const retryMux = (job: JobId) => send({ cmd: "retryMux", job });
export const clearCompleted = () => send({ cmd: "clearCompleted" });
export const probe = (url: string) =>
  send({ cmd: "probe", envelope: envelope(url) });
export const submit = (spec: JobSpec) => send({ cmd: "submit", spec });
export const save = (settings: Settings) => send({ cmd: "setSettings", settings });

/**
 * A URL the user typed or dropped, as a `RequestEnvelope`.
 *
 * The envelope from the extension is the real article — headers, cookies, the page it came
 * from, captured verbatim, which is what makes a handoff work. This is the other case: a
 * bare URL with nothing behind it. Everything optional is left empty rather than invented,
 * because a guessed `Referer` that the server does not expect is worse than none.
 */
function envelope(url: string) {
  return {
    url,
    finalUrl: null,
    method: "GET",
    headers: [] as Array<[string, string]>,
    cookies: null,
    bodyBase64: null,
    mimeType: null,
    contentLength: null,
    filenameHint: null,
    tabId: null,
    pageUrl: null,
    pageTitle: null,
    capturedAt: Math.floor(Date.now() / 1000),
  };
}

/** The tray's two entries. The window owns the list, so the window answers them. */
export async function pauseAll(): Promise<void> {
  for (const job of queue.jobs) {
    if (job.view.state.kind === "downloading") await pause(job.id);
  }
}

export async function resumeAll(): Promise<void> {
  for (const job of queue.jobs) {
    if (job.view.state.kind === "paused") await resume(job.id);
  }
}

/** Opens the finished file with whatever the OS thinks owns it. */
export async function open(path: string): Promise<void> {
  try {
    await openPath(path);
  } catch {
    queue.problem = "That file isn't there any more.";
  }
}

/** Shows the file in Explorer or Finder, selected. The usual "where did it go" answer. */
export async function reveal(path: string): Promise<void> {
  try {
    await revealItemInDir(path);
  } catch {
    queue.problem = "That file isn't there any more.";
  }
}
