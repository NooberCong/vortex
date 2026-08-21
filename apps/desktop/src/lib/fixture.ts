import type {
  Category,
  Command,
  Event,
  JobId,
  JobState,
  JobView,
  ProgressFrame,
  Settings,
  SummaryFrame,
} from "@vortex/proto";

/**
 * A scripted `vortexd`, for `vite dev` in a plain browser.
 *
 * It exists to make the quality floor measurable. "60 fps with 40 jobs listed" and "idle
 * CPU under 0.5%" are the exit criteria for this phase (05 §Quality floor), and neither
 * can be looked at on a screen that first needs forty real downloads and a hostile CDN.
 *
 * So it is a simulation rather than a recording: workers hold contiguous leases, finish
 * them, and steal the back half of the largest remaining region from whoever is slowest —
 * the actual scheduler, in miniature. That matters, because the thing being checked is
 * whether the segment map reads correctly while leases change hands, and a canned sequence
 * of frames would only prove the renderer can draw a canned sequence of frames.
 *
 * `import.meta.env.DEV` gates the only import of this module, so none of it reaches a
 * production bundle. `tests/fixture.test.ts` checks that by reading the built output.
 */

const BLOCK = 1 << 20;
/** 20 Hz for the expanded row, 2 Hz for the list (01 §IPC). */
const DETAIL_MS = 50;
const SUMMARY_MS = 500;

interface Worker {
  lane: number;
  /** Next block this worker will complete, and one past the end of its lease. */
  at: number;
  end: number;
  /** The rate this worker settles around; `bps` is that rate with the wobble applied. */
  base: number;
  bps: number;
  spark: number[];
  stealingFrom: boolean;
}

class Simulated {
  view: JobView;
  /** `-1` not started, `0` complete, `n` in flight by worker `n`. */
  blocks: Int16Array;
  workers: Worker[] = [];
  bps = 0;
  /** Set once when the simulation finishes a job, so the completion is announced once. */
  announce = false;
  /** Simulated seconds, which drives the per-worker rate wobble. */
  age = 0;

  constructor(view: JobView, blocks: number) {
    this.view = view;
    this.blocks = new Int16Array(blocks).fill(-1);
  }

  get running(): boolean {
    return this.view.state.kind === "downloading";
  }

  start(count: number, speed: number): void {
    const size = Math.floor(this.blocks.length / count);
    this.workers = Array.from({ length: count }, (_, i) => ({
      lane: i + 1,
      at: i * size,
      end: i === count - 1 ? this.blocks.length : (i + 1) * size,
      // A spread of a bit under 3× across workers, which is what a real fanout against a
      // CDN with a per-connection cap looks like.
      base: speed * (0.5 + ((i * 7) % 10) / 8),
      bps: speed * (0.5 + ((i * 7) % 10) / 8),
      spark: [],
      stealingFrom: false,
    }));
  }

  step(seconds: number): void {
    if (!this.running) return;
    this.age += seconds;
    for (const worker of this.workers) {
      // A real connection wanders. Two out-of-phase sines per worker give the sparkline
      // something to show and the steal logic something to prefer, without a random source
      // — which would make the same fixture look different on every reload.
      const wobble =
        1 + 0.25 * Math.sin(this.age / 3 + worker.lane) + 0.12 * Math.sin(this.age / 1.1 + worker.lane * 2);
      worker.bps = worker.base * Math.max(0.15, wobble);
      const advance = (worker.bps * seconds) / BLOCK;
      // Fractional progress is carried in `at`; the floor is what is actually complete.
      const before = Math.floor(worker.at);
      worker.at = Math.min(worker.end, worker.at + advance);
      for (let b = before; b < Math.floor(worker.at); b += 1) this.blocks[b] = 0;
      // In flight is the whole remaining lease, not the block currently arriving — that is
      // what `Scheduler::frame` puts on the wire, and it is why the map shows each worker's
      // territory rather than a moving dot.
      for (let b = Math.floor(worker.at); b < worker.end; b += 1) this.blocks[b] = worker.lane;

      worker.spark.push(worker.bps);
      if (worker.spark.length > 24) worker.spark.shift();
      worker.stealingFrom = false;
    }

    for (const idle of this.workers.filter((w) => w.at >= w.end)) this.steal(idle);

    this.bps = this.workers
      .filter((w) => w.at < w.end)
      .reduce((sum, w) => sum + w.bps, 0);
    this.view.completed = Math.min(
      this.view.total ?? 0,
      this.blocks.reduce((n, b) => (b === 0 ? n + 1 : n), 0) * BLOCK,
    );
    if (this.workers.every((w) => w.at >= w.end)) this.finish();
  }

  /**
   * The satisfying one: a worker that has finished takes the back half of the largest
   * outstanding lease, and the map visibly hands work from the slow lane to the fast one
   * (05 §Signature).
   */
  private steal(idle: Worker): void {
    let victim: Worker | null = null;
    let biggest = 0;
    for (const worker of this.workers) {
      const left = worker.end - worker.at;
      // Two blocks is the floor: splitting anything smaller costs a connection setup to
      // save less than it costs.
      if (left > biggest && left > 2 && worker !== idle) {
        biggest = left;
        victim = worker;
      }
    }
    if (!victim) return;
    const split = Math.ceil(victim.at + biggest / 2);
    idle.at = split;
    idle.end = victim.end;
    victim.end = split;
    victim.stealingFrom = true;
  }

  private finish(): void {
    this.view.state = { kind: "completed" };
    this.view.completed = this.view.total ?? this.view.completed;
    this.view.finishedAt = Math.floor(Date.now() / 1000);
    this.bps = 0;
    this.announce = true;
  }

  /** RLE, exactly as `ProgressFrame` carries it: `(start, len, owner)`, complete is 0. */
  runs(): SummaryFrame["runs"] {
    const runs: SummaryFrame["runs"] = [];
    let start = 0;
    let owner = this.blocks[0] ?? -1;
    for (let i = 1; i <= this.blocks.length; i += 1) {
      const here = i < this.blocks.length ? this.blocks[i]! : -2;
      if (here === owner) continue;
      if (owner >= 0) runs.push([start, i - start, owner]);
      start = i;
      owner = here;
    }
    return runs;
  }

  summary(): SummaryFrame {
    return {
      total: this.view.total,
      completed: this.view.completed,
      bps: this.bps,
      etaSecs: this.eta(),
      connections: this.workers.filter((w) => w.at < w.end).length,
      blocks: this.blocks.length,
      // Coarsened exactly as `vortexd`'s `frames::summary` does. Getting this wrong would
      // make the fixture flattering rather than faithful: the collapsed bar really is
      // "complete or moving", and the spectrum really does belong to the expanded row.
      runs: coarsen(this.runs(), SUMMARY_RUNS),
    };
  }

  detail(): ProgressFrame {
    return {
      ...this.summary(),
      runs: this.runs(),
      blockSize: BLOCK,
      workers: this.workers
        .filter((w) => w.at < w.end)
        .map((w) => ({
          lane: w.lane,
          bps: w.bps,
          spark: [...w.spark],
          stealingFrom: w.stealingFrom,
        })),
    };
  }

  private eta(): number | null {
    if (!this.view.total || this.bps <= 0) return null;
    return Math.round((this.view.total - this.view.completed) / this.bps);
  }
}

/** `vortexd` sends at most this many runs in a summary; six pixels cannot resolve more. */
const SUMMARY_RUNS = 24;

/**
 * The TypeScript twin of `crates/vortexd/src/frames.rs`.
 *
 * Collapses per-worker ownership to "done / moving", then absorbs the shortest runs until
 * the frame is small enough to send twice a second. Every block stays accounted for; only
 * the boundaries move.
 */
function coarsen(runs: SummaryFrame["runs"], limit: number): SummaryFrame["runs"] {
  const out: SummaryFrame["runs"] = [];
  for (const [start, len, owner] of runs) {
    const flag = owner === 0 ? 0 : 1;
    const last = out[out.length - 1];
    if (last && last[2] === flag && last[0] + last[1] === start) last[1] += len;
    else out.push([start, len, flag]);
  }
  while (out.length > limit) {
    let victim = 1;
    for (let i = 2; i < out.length; i += 1) if (out[i]![1] < out[victim]![1]) victim = i;
    out[victim - 1]![1] += out[victim]![1];
    out.splice(victim, 1);
    if (victim < out.length && out[victim - 1]![2] === out[victim]![2]) {
      out[victim - 1]![1] += out[victim]![1];
      out.splice(victim, 1);
    }
  }
  return out;
}

const CATALOGUE: Array<{
  filename: string;
  host: string;
  category: Category;
  total: number;
  state: JobState;
  connections?: number;
  speed?: number;
  media?: JobView["media"];
}> = [
  { filename: "ubuntu-24.04.2-desktop-amd64.iso", host: "releases.ubuntu.com", category: "Archives", total: 4_900_000_000, state: { kind: "downloading" }, connections: 12, speed: 7_000_000 },
  { filename: "Session 3 — Distributed Consensus.mkv", host: "cdn.confvideo.io", category: "Video", total: 1_820_000_000, state: { kind: "muxing" }, media: { segmentsTotal: 1042, segmentsDone: 1042, segmentsMissing: 0, muxPercent: 62, containerNote: "Saved as MKV — MP4 can't hold Opus audio." } },
  { filename: "Annual Report 2025.pdf", host: "investors.example.com", category: "Documents", total: 18_200_000, state: { kind: "paused" } },
  { filename: "blender-4.2.1-windows-x64.msi", host: "mirror.clarkson.edu", category: "Programs", total: 380_000_000, state: { kind: "downloading" }, connections: 8, speed: 5_200_000 },
  { filename: "The Long Interview - part 2.mp4", host: "video.example.org", category: "Video", total: 940_000_000, state: { kind: "stalled", reason: "The server stopped responding. Retrying in 12s." } },
  { filename: "dataset-2025-Q3.tar.zst", host: "data.openml.org", category: "Archives", total: 12_400_000_000, state: { kind: "downloading" }, connections: 16, speed: 9_500_000 },
  { filename: "Ambient Works — remaster.flac", host: "files.label.fm", category: "Audio", total: 412_000_000, state: { kind: "queued" } },
  { filename: "Nightly build 8f21c4.zip", host: "artifacts.ci.example", category: "Archives", total: 96_000_000, state: { kind: "needsDecision", decision: { kind: "diskFull", needed: 2_100_000_000, drive: "D:" } } },
  { filename: "photogrammetry-scan-047.zip", host: "storage.googleapis.com", category: "Archives", total: 2_300_000_000, state: { kind: "downloading" }, connections: 6, speed: 3_100_000 },
  { filename: "keynote-2160p.mp4", host: "stream.example.tv", category: "Video", total: 3_400_000_000, state: { kind: "failed", error: "The link expired." } },
];

/**
 * The queue, built by varying the catalogue rather than repeating it.
 *
 * More than the forty of the frame-rate case, and the list still renders exactly forty:
 * one page (`store.svelte.ts`, `PAGE`). Everything past the first ten is completed, so the
 * measurement is unchanged — the same forty rows, the same handful of them moving — while
 * the tail is long enough that `vite dev` shows the paging footer at the bottom of the
 * Done section instead of a list that happens to end.
 */
function catalogue(): Simulated[] {
  const jobs: Simulated[] = [];
  for (let i = 0; i < 96; i += 1) {
    const base = CATALOGUE[i % CATALOGUE.length]!;
    const round = Math.floor(i / CATALOGUE.length);
    const total = Math.round(base.total * (1 + round * 0.37));
    const state: JobState = round === 0 ? base.state : { kind: "completed" };
    const view: JobView = {
      id: (i + 1) as JobId,
      filename: round === 0 ? base.filename : `${round + 1} · ${base.filename}`,
      destPath: `D:\\Downloads\\${base.filename}`,
      url: `https://${base.host}/${base.filename}`,
      host: base.host,
      category: base.category,
      state,
      priority: "Normal",
      total,
      completed: state.kind === "completed" ? total : 0,
      mode: "Parallel",
      protocol: i % 3 === 0 ? "H3" : "H2",
      addresses: 1 + (i % 4),
      createdAt: Math.floor(Date.now() / 1000) - i * 900,
      finishedAt: state.kind === "completed" ? Math.floor(Date.now() / 1000) - i * 600 : null,
      verified: i % 5 === 0 ? { algorithm: "SHA-256", ok: true } : null,
      retries: { total: i % 4, recovered: i % 4 },
      media: base.media ?? null,
    };
    const job = new Simulated(view, Math.max(8, Math.ceil(total / BLOCK)));
    if (state.kind === "completed") {
      job.blocks.fill(0);
    } else if (state.kind === "downloading") {
      job.start(base.connections ?? 8, base.speed ?? 4_000_000);
      // Start part-way in, so the list opens on something worth looking at rather than on
      // forty bars at zero.
      for (let t = 0; t < 20 + (i % 30); t += 1) job.step(1);
    } else if (state.kind === "paused" || state.kind === "stalled") {
      const done = Math.floor(job.blocks.length * 0.31);
      job.blocks.fill(0, 0, done);
      view.completed = done * BLOCK;
    } else if (state.kind === "muxing") {
      job.blocks.fill(0);
      view.completed = total;
    }
    jobs.push(job);
  }
  return jobs;
}

const SETTINGS: Settings = {
  downloadDir: "D:\\Downloads",
  categoryDirs: { Video: "Video", Audio: "Music", Archives: "Archives", Documents: "Documents" },
  maxConnections: 16,
  maxConcurrentJobs: 4,
  globalSpeedLimit: 0,
  enableH3: true,
  enableCapture: true,
  siteOptouts: ["figma.com"],
  minCaptureBytes: 1_000_000,
  defaultMediaHeight: 1080,
  container: "Auto",
  subtitles: true,
  theme: "system",
  reducedMotion: false,
  clipboardMonitor: true,
  autostart: true,
  writeBufferBudget: 268_435_456,
};

export function fixture() {
  const jobs = catalogue();
  const listeners = { event: new Set<(e: Event) => void>() };
  let watching: JobId | null = null;

  const emit = (event: Event) => listeners.event.forEach((fn) => fn(event));
  const find = (id: JobId) => jobs.find((j) => j.view.id === id);

  setInterval(() => {
    for (const job of jobs) job.step(SUMMARY_MS / 1000);
    for (const job of jobs) {
      if (job.running) {
        emit({ event: "jobSummary", job: job.view.id, frame: job.summary() });
      } else if (job.announce) {
        job.announce = false;
        emit({ event: "jobStateChanged", job: job.view.id, state: job.view.state });
        emit({
          event: "jobFinished",
          job: job.view.id,
          outcome: {
            kind: "completed",
            path: job.view.destPath,
            bytes: job.view.completed,
            elapsedSecs: 96,
            verified: job.view.verified ?? null,
          },
        });
      }
    }
  }, SUMMARY_MS);

  setInterval(() => {
    const job = watching === null ? null : find(watching);
    if (job?.running) emit({ event: "jobProgress", job: job.view.id, frame: job.detail() });
  }, DETAIL_MS);

  return {
    async send(command: Command) {
      if (command.cmd === "list") {
        emit({ event: "jobs", jobs: jobs.map((j) => j.view) });
      } else if (command.cmd === "getSettings") {
        emit({ event: "settingsChanged", settings: SETTINGS });
      } else if (command.cmd === "pause" || command.cmd === "resume") {
        const job = find(command.job);
        if (job && job.view.state.kind !== "completed") {
          job.view.state = command.cmd === "pause" ? { kind: "paused" } : { kind: "downloading" };
          if (command.cmd === "resume" && job.workers.length === 0) job.start(8, 4_000_000);
          emit({ event: "jobStateChanged", job: job.view.id, state: job.view.state });
        }
      } else if (command.cmd === "remove" || command.cmd === "cancel") {
        const index = jobs.findIndex((j) => j.view.id === command.job);
        if (index >= 0) {
          jobs.splice(index, 1);
          emit({ event: "jobRemoved", job: command.job });
        }
      } else if (command.cmd === "probe") {
        emit({
          event: "probed",
          result: {
            url: command.envelope.url,
            finalUrl: command.envelope.url,
            filename: command.envelope.url.split("/").pop() || "download",
            host: new URL(command.envelope.url).host,
            size: 4_900_000_000,
            mimeType: "application/octet-stream",
            mode: "Parallel",
            resumable: true,
            category: "Archives",
            suggestedDir: SETTINGS.downloadDir,
          },
        });
      }
    },
    async watch(job: JobId | null) {
      watching = job;
    },
    async connected() {
      return true;
    },
    async tooltip() {},
    async onEvent(handler: (e: Event) => void) {
      listeners.event.add(handler);
      // The real bridge sends `List` and `GetSettings` as part of its handshake (see
      // `src-tauri/src/link.rs`). Doing the same here is what makes the fixture a stand-in
      // for the connection rather than for the daemon behind it.
      queueMicrotask(() => {
        handler({ event: "jobs", jobs: jobs.map((j) => j.view) });
        handler({ event: "settingsChanged", settings: SETTINGS });
      });
      return () => listeners.event.delete(handler);
    },
    async onLink(handler: (up: boolean) => void) {
      queueMicrotask(() => handler(true));
      return () => {};
    },
    async onTray() {
      return () => {};
    },
  };
}
