<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";

  import * as act from "$lib/actions";
  import { sample } from "$lib/aggregate.svelte";
  import { primaryAction } from "$lib/copy";
  import { daemon, native } from "$lib/ipc";
  import { match, type Intent } from "$lib/keys";
  import { announce } from "$lib/notify";
  import { expand, queue } from "$lib/store.svelte";
  import { applyAppearance } from "$lib/theme";
  import JobList from "./components/JobList.svelte";
  import NewDownload from "./components/NewDownload.svelte";
  import Settings from "./components/Settings.svelte";
  import Sidebar from "./components/Sidebar.svelte";
  import Titlebar from "./components/Titlebar.svelte";

  /**
   * Fades out the boot splash in `index.html` and removes it.
   *
   * It is held for a minimum beat rather than dismissed the instant the daemon answers:
   * `performance.now()` is roughly time-since-navigation, so a warm start that answers in
   * 40 ms still gets the figure drawn instead of a flash of something half-finished. A
   * cold start has already spent the beat and pays nothing.
   *
   * Idempotent, and it removes the node — a fixed overlay left in the tree is a fixed
   * overlay that will one day swallow a click.
   */
  function dismissSplash(): void {
    const splash = document.getElementById("splash");
    if (!splash || splash.classList.contains("done")) return;
    setTimeout(
      () => {
        splash.classList.add("done");
        splash.addEventListener("transitionend", () => splash.remove(), { once: true });
        // `transitionend` does not fire for a tab that is not compositing.
        setTimeout(() => splash.remove(), 600);
      },
      Math.max(0, 620 - performance.now()),
    );
  }

  /**
   * The window.
   *
   * Everything that is process-wide is wired here and nowhere else: one subscription to
   * the event stream, one sampler for the aggregate readout, one keyboard handler. A
   * component that needed its own would be a second thing to unsubscribe and a second
   * place for the order of two events to matter.
   */

  let adding = $state(false);
  let settings = $state(false);
  let searching = $state(false);
  let dropping = $state(false);
  /** The daemon has answered, one way or the other. Until then the splash stays up. */
  let booted = $state(false);

  onMount(() => {
    const stops: Array<() => void> = [];

    void daemon
      .onEvent((event) => {
        queue.apply(event);
        void announce(event);
      })
      .then((off) => {
        stops.push(off);
        // The link asks for the list and the settings in its handshake, and when a daemon
        // is *already running* — autostart, or the browser's native host started one —
        // that handshake completes before this webview exists. Both answers are then
        // emitted into a window with nothing listening, and they are gone: an empty list
        // that looks like a lost queue, and a Settings sheet that waits forever.
        //
        // So the window asks again, now that there is somewhere for the answer to land.
        // Both commands are idempotent, and a send that fails because the link is still
        // coming up costs nothing — the handshake will deliver them, and this time the
        // listener is already attached.
        void daemon.send({ cmd: "list" }).catch(() => {});
        void daemon.send({ cmd: "getSettings" }).catch(() => {});
      });
    void daemon.onLink((up) => (queue.connected = up)).then((off) => stops.push(off));
    void daemon
      .onTray((intent) => void (intent === "pauseAll" ? act.pauseAll() : act.resumeAll()))
      .then((off) => stops.push(off));
    void daemon
      .connected()
      .then((up) => (queue.connected = up))
      .finally(() => (booted = true));

    // A daemon that never answers must not mean a window that never opens. "Not
    // connected" is a state this app already knows how to draw — the list says so — and
    // showing it is better than holding a splash over it.
    const giveUp = setTimeout(() => (booted = true), 2500);
    stops.push(() => clearTimeout(giveUp));

    stops.push(sample(native ? getCurrentWindow() : null));

    // The shell paints the window's corner, so the shell has to know when there is no
    // corner to paint: maximised, the window is the work area and a radius is a notch of
    // desktop in each of the four edges. `onResized` is the signal — maximising, restoring
    // and snapping all arrive as one.
    if (native) {
      const frame = getCurrentWindow();
      const corner = (): void => {
        void frame.isMaximized().then((max) => {
          document.documentElement.toggleAttribute("data-maximised", max);
        });
      };
      corner();
      void frame.onResized(corner).then((off) => stops.push(off));
    }

    return () => stops.forEach((stop) => stop());
  });

  /** Hand the window over to the real UI once there is a real UI to hand it to. */
  $effect(() => {
    if (booted) dismissSplash();
  });

  /** Appearance is a setting like any other, so it is applied when the daemon says so. */
  $effect(() => {
    if (queue.settings) applyAppearance(queue.settings);
  });

  /** Dismissed on its own after a moment; an error the user did not read twice is noise. */
  $effect(() => {
    if (!queue.problem) return;
    const timer = setTimeout(() => (queue.problem = null), 6000);
    return () => clearTimeout(timer);
  });

  function step(delta: number): void {
    const rows = queue.visible;
    if (rows.length === 0) return;
    const at = rows.findIndex((job) => job.id === queue.selected);
    const next = at < 0 ? (delta > 0 ? 0 : rows.length - 1) : at + delta;
    const row = rows[Math.min(rows.length - 1, Math.max(0, next))];
    if (row) queue.selected = row.id;
  }

  function dispatch(intent: Intent, event: KeyboardEvent): void {
    const selected = queue.selected === null ? null : queue.get(queue.selected);

    switch (intent) {
      case "new":
        settings = false;
        adding = true;
        break;
      case "settings":
        adding = false;
        settings = true;
        break;
      case "search":
        searching = true;
        break;
      case "close":
        // One escape, one thing: the sheet if there is one, then the filter, then the
        // expanded row. Closing all three at once loses more than the user asked to lose.
        if (adding || settings) (adding = false), (settings = false);
        else if (searching) (searching = false), (queue.search = "");
        else if (queue.expanded !== null) void expand(null);
        else return;
        break;
      case "toggle": {
        const action = selected && primaryAction(selected.view.state);
        if (!selected || !action) return;
        void (action === "pause" ? act.pause(selected.id) : act.resume(selected.id));
        break;
      }
      case "remove":
        if (!selected) return;
        void act.remove(selected.id);
        break;
      case "expand":
        if (!selected) return;
        void expand(selected.id);
        break;
      case "up":
        step(-1);
        break;
      case "down":
        step(1);
        break;
    }
    event.preventDefault();
  }

  function keydown(event: KeyboardEvent): void {
    const intent = match(event);
    if (intent) dispatch(intent, event);
  }

  /**
   * A link dropped anywhere on the window (05 §Platform polish).
   *
   * It goes through the same probe as a typed one, so the sheet can say what the file is
   * before anything is committed — a drop is not a decision to download, it is a decision
   * to look.
   */
  function drop(event: DragEvent): void {
    event.preventDefault();
    dropping = false;
    const text = event.dataTransfer?.getData("text/uri-list") || event.dataTransfer?.getData("text");
    const link = text?.split(/\r?\n/).find((line) => /^https?:\/\//i.test(line.trim()));
    if (!link) return;
    settings = false;
    adding = true;
    queue.probed = null;
    void act.probe(link.trim());
  }
</script>

<svelte:window
  onkeydown={keydown}
  ondragover={(e) => {
    e.preventDefault();
    dropping = true;
  }}
  ondragleave={() => (dropping = false)}
  ondrop={drop}
/>

<div class="app" class:dropping>
  <Titlebar
    {searching}
    onAdd={() => ((settings = false), (adding = true))}
    onSearch={() => (searching = !searching)}
  />

  <main>
    <Sidebar settingsOpen={settings} onSettings={() => (settings = !settings)} />
    <JobList />
  </main>

  {#if adding}
    <NewDownload onClose={() => (adding = false)} />
  {/if}

  {#if settings}
    <Settings onClose={() => (settings = false)} />
  {/if}

  {#if queue.problem}
    <!--
      What happened, in the daemon's own words. It is already a sentence in the interface's
      voice — `EngineError::user_message` writes it — so this only has to show it and get
      out of the way.
    -->
    <div class="toast" role="status">
      <span>{queue.problem}</span>
      <button class="button quiet" onclick={() => (queue.problem = null)}>Dismiss</button>
    </div>
  {/if}
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg);
    /* Frameless, with the corner the window itself does not have (05 §Layout). Zero when
       maximised, where the window meets the screen edge — see `app.css`. */
    border-radius: var(--shell-radius);
    overflow: hidden;
  }

  main {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  /* A dragged link gets one hairline, inside the window. Not a full-screen overlay saying
     "drop here" — the user is already holding the thing and knows what they are doing. */
  .dropping::after {
    content: "";
    position: fixed;
    inset: 0;
    border: 2px solid var(--focus);
    border-radius: var(--shell-radius);
    pointer-events: none;
  }

  .toast {
    position: fixed;
    left: 50%;
    bottom: var(--s5);
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: var(--s4);
    max-width: min(560px, calc(100vw - var(--s7)));
    padding: var(--s2) var(--s2) var(--s2) var(--s4);
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
    animation: rise var(--calm) var(--ease);
  }

  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, 8px);
    }
  }
</style>
