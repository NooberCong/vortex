import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";

import type { Command, JobId, JobSpec, Resolution, Settings } from "@vortex/proto";

import { daemon } from "./ipc";
import { queue, type Job } from "./store.svelte";

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

/**
 * The × on a row, and the Delete key. Asks first when the answer costs a file.
 *
 * Removing a finished download is two things the interface has always spelled as one: the
 * row goes, and the file it produced either stays or does not. That is a question only the
 * user can answer, and the moment to ask is here — before anything happens, while the file
 * still has a name and a size to put in the sentence.
 *
 * Only for a completed job. Everything else has no finished file at stake: the engine
 * downloads into `.vxpart` and renames at the very end, and the daemon's `retire` deletes
 * the partial work whichever way the question is answered. Asking anyway would be a dialog
 * whose two branches do the same thing, which is how an app teaches people to click
 * through its dialogs without reading them.
 */
export function requestRemove(job: Job): void {
  if (job.view.state.kind === "completed") queue.removing = job.id;
  else void remove(job.id);
}
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
  } catch (e) {
    queue.problem = whyNot(e);
  }
}

/** Shows the file in Explorer or Finder, selected. The usual "where did it go" answer. */
export async function reveal(path: string): Promise<void> {
  try {
    await revealItemInDir(path);
  } catch (e) {
    queue.problem = whyNot(e);
  }
}

/**
 * What actually went wrong, rather than the one guess.
 *
 * These two used to answer every failure with "That file isn't there any more.", which is
 * the single most expensive thing they could have said: it is a confident, specific claim
 * about the user's disk, and when it is wrong it sends them to a folder to look for a file
 * that is sitting in it. Opening fails for at least three reasons and only one of them is
 * the file being gone - the others are the path falling outside the opener's allow-list,
 * which is our packaging bug and not theirs, and the system having no application
 * registered for the extension, which is neither.
 *
 * So the error is read rather than assumed, and where it is not recognised it is passed
 * through. An unfamiliar sentence from the plugin is worth more than a familiar one we
 * made up.
 */
function whyNot(e: unknown): string {
  const detail = String((e as { message?: string })?.message ?? e ?? "").trim();

  // `Error::ForbiddenPath`, verbatim from the plugin. Nothing the user can do about it.
  if (/not allowed to open path/i.test(detail)) {
    return "Vortex isn't allowed to open that folder. Use Show in folder - and report this.";
  }
  if (/no such file|not found|cannot find|does not exist/i.test(detail)) {
    return "That file isn't there any more.";
  }
  if (/no application|no such application|not associated|unknown program/i.test(detail)) {
    return "Nothing on this computer is set up to open that kind of file.";
  }
  return detail ? `Couldn't open that file. ${detail}` : "Couldn't open that file.";
}
