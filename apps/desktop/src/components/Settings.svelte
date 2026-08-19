<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { bytes, PROTOCOL_VERSION } from "@vortex/proto";
  import type { ContainerPreference, Settings, Theme } from "@vortex/proto";

  import * as act from "$lib/actions";
  import { native } from "$lib/ipc";
  import { applyTheme } from "$lib/theme";
  import { queue } from "$lib/store.svelte";
  import Select from "./Select.svelte";
  import Sheet from "./Sheet.svelte";
  import Toggle from "./Toggle.svelte";

  /**
   * One scrolling column, sectioned, no tabs (05 §Settings).
   *
   * Tabs in a settings screen are a way of hiding that there are too many settings. There
   * are six sections here and they fit in one scroll, which is the constraint keeping the
   * count honest.
   *
   * Every control writes the whole `Settings` back to the daemon, which owns the file and
   * hot-reloads it. Nothing here is applied locally and then persisted later — the theme
   * included, which is why a theme change survives a restart without this component
   * knowing where anything is stored.
   */

  interface Props {
    onClose: () => void;
  }

  const { onClose }: Props = $props();

  // A working copy. Editing `queue.settings` directly would fight the daemon's echo
  // halfway through typing a path.
  let draft = $state<Settings | null>(null);

  // Adopted once, when the daemon first answers. Re-adopting on every echo would undo an
  // edit that was still being typed when the previous one came back.
  $effect(() => {
    const live = queue.settings;
    if (live && !draft) draft = structuredClone(live);
  });

  const HEIGHTS = [360, 480, 720, 1080, 1440, 2160].map((height) => ({
    value: height,
    label: `${height}p`,
  }));
  const THEMES: { value: Theme; label: string }[] = [
    { value: "system", label: "Match the system" },
    { value: "light", label: "Paper" },
    { value: "dark", label: "Ink" },
  ];
  const CONTAINERS: { value: ContainerPreference; label: string }[] = [
    { value: "Auto", label: "Choose automatically" },
    { value: "Mp4", label: "MP4" },
    { value: "Mkv", label: "MKV" },
  ];

  function commit(): void {
    if (draft) void act.save($state.snapshot(draft) as Settings);
  }

  function set<K extends keyof Settings>(key: K, value: Settings[K]): void {
    if (!draft) return;
    draft[key] = value;
    if (key === "theme") applyTheme(value as Theme);
    commit();
  }

  async function pickFolder(): Promise<void> {
    if (!native || !draft) return;
    const chosen = await open({ directory: true, defaultPath: draft.downloadDir });
    if (typeof chosen === "string") set("downloadDir", chosen);
  }

  const without = (list: string[] | undefined, site: string) =>
    (list ?? []).filter((entry) => entry !== site);

  /** Megabytes per second in the field, bytes per second on the wire. 0 is unlimited. */
  const limit = $derived(draft ? Math.round((draft.globalSpeedLimit / 1_000_000) * 10) / 10 : 0);
</script>

<Sheet title="Settings" {onClose} wide>
  {#if !draft}
    <p class="meta">Waiting for Vortex…</p>
  {:else}
    <section>
      <h3 class="micro">Downloads</h3>

      <div class="setting">
        <label for="dir">Save files to</label>
        <div class="control">
          <input
            id="dir"
            class="input num"
            spellcheck="false"
            value={draft.downloadDir}
            onchange={(e) => set("downloadDir", e.currentTarget.value)}
          />
          <button class="button" onclick={pickFolder} disabled={!native}>Browse</button>
        </div>
      </div>

      <div class="setting">
        <span id="autostart-label">Start Vortex when I sign in</span>
        <Toggle
          label="Start Vortex when I sign in"
          checked={draft.autostart}
          onchange={(v) => set("autostart", v)}
        />
      </div>
      <p class="note">
        Downloads keep going with the window closed. This only decides whether Vortex is
        already running when you need it.
      </p>

      <div class="setting">
        <span>Watch the clipboard for links</span>
        <Toggle
          label="Watch the clipboard for links"
          checked={draft.clipboardMonitor}
          onchange={(v) => set("clipboardMonitor", v)}
        />
      </div>
    </section>

    <section>
      <h3 class="micro">Speed</h3>

      <div class="setting">
        <label for="conns">Maximum connections</label>
        <div class="control">
          <input
            id="conns"
            type="range"
            min="1"
            max="32"
            value={draft.maxConnections}
            oninput={(e) => set("maxConnections", Number(e.currentTarget.value))}
          />
          <span class="value num">{draft.maxConnections}</span>
        </div>
      </div>
      <!--
        The adaptive controller is the feature. A user who sets 32 and then sees 12 in use
        needs to know that is correct and not a bug (05 §Settings).
      -->
      <p class="note">Vortex adjusts automatically per server. This is the ceiling.</p>

      <div class="setting">
        <label for="jobs">Downloads at once</label>
        <div class="control">
          <input
            id="jobs"
            type="range"
            min="1"
            max="10"
            value={draft.maxConcurrentJobs}
            oninput={(e) => set("maxConcurrentJobs", Number(e.currentTarget.value))}
          />
          <span class="value num">{draft.maxConcurrentJobs}</span>
        </div>
      </div>

      <div class="setting">
        <label for="limit">Speed limit</label>
        <div class="control">
          <input
            id="limit"
            class="input num narrow"
            type="number"
            min="0"
            step="0.5"
            value={limit}
            onchange={(e) =>
              set("globalSpeedLimit", Math.max(0, Number(e.currentTarget.value)) * 1_000_000)}
          />
          <span class="unit meta">MB/s · 0 is unlimited</span>
        </div>
      </div>
    </section>

    <section>
      <h3 class="micro">Browser</h3>

      <div class="setting">
        <span>Catch downloads from the browser</span>
        <Toggle
          label="Catch downloads from the browser"
          checked={draft.enableCapture}
          onchange={(v) => set("enableCapture", v)}
        />
      </div>
      <p class="note">
        The extension hands a download to Vortex only when Vortex is running. If it is not,
        the browser keeps the download and nothing is lost.
      </p>

      <div class="setting">
        <label for="floor">Ignore files smaller than</label>
        <div class="control">
          <input
            id="floor"
            class="input num narrow"
            type="number"
            min="0"
            step="0.1"
            value={Math.round((draft.minCaptureBytes / 1_000_000) * 10) / 10}
            onchange={(e) =>
              set("minCaptureBytes", Math.max(0, Number(e.currentTarget.value)) * 1_000_000)}
          />
          <span class="unit meta">MB</span>
        </div>
      </div>

      {#if draft.siteOptouts.length > 0}
        <div class="setting stack">
          <span>Sites you turned Vortex off on</span>
          <ul class="chips">
            {#each draft.siteOptouts as site (site)}
              <li class="chip">
                <span class="num">{site}</span>
                <button
                  class="button quiet tiny"
                  aria-label="Turn Vortex back on for {site}"
                  onclick={() => set("siteOptouts", without(draft?.siteOptouts, site))}
                >
                  ×
                </button>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
    </section>

    <section>
      <h3 class="micro">Media</h3>

      <div class="setting">
        <label for="quality">Preferred quality</label>
        <div class="pick">
          <Select
            id="quality"
            options={HEIGHTS}
            value={draft.defaultMediaHeight}
            onchange={(height) => set("defaultMediaHeight", height)}
          />
        </div>
      </div>
      <p class="note">
        Vortex offers the rung nearest what is actually playing, and never picks the
        highest one for you.
      </p>

      <div class="setting">
        <label for="container">Container</label>
        <div class="pick">
          <Select
            id="container"
            options={CONTAINERS}
            value={draft.container}
            onchange={(container) => set("container", container)}
          />
        </div>
      </div>

      <div class="setting">
        <span>Save subtitles when they are there</span>
        <Toggle
          label="Save subtitles when they are there"
          checked={draft.subtitles}
          onchange={(v) => set("subtitles", v)}
        />
      </div>
    </section>

    <section>
      <h3 class="micro">Advanced</h3>

      <div class="setting">
        <label for="theme">Appearance</label>
        <div class="pick">
          <Select id="theme" options={THEMES} value={draft.theme} onchange={(theme) => set("theme", theme)} />
        </div>
      </div>

      <div class="setting">
        <span>Reduce motion</span>
        <Toggle
          label="Reduce motion"
          checked={draft.reducedMotion}
          onchange={(v) => set("reducedMotion", v)}
        />
      </div>

      <div class="setting">
        <span>Use HTTP/3 where the server offers it</span>
        <Toggle
          label="Use HTTP/3 where the server offers it"
          checked={draft.enableH3}
          onchange={(v) => set("enableH3", v)}
        />
      </div>
      <p class="note">
        Vortex falls back to HTTP/2 by itself if a connection misbehaves. Turning this off
        skips the attempt.
      </p>

      <div class="setting">
        <span>Write buffer</span>
        <span class="value num">{bytes(draft.writeBufferBudget)}</span>
      </div>
    </section>

    <section>
      <h3 class="micro">About</h3>
      <dl class="about meta">
        <dt>Vortex</dt>
        <dd class="num">0.1.0</dd>
        <dt>Protocol</dt>
        <dd class="num">{PROTOCOL_VERSION}</dd>
        <dt>Downloads</dt>
        <dd class="num">{queue.jobs.length} in the list</dd>
      </dl>
      <div class="setting">
        <span>Remove everything that has finished</span>
        <button class="button" onclick={() => act.clearCompleted()}>Clear finished</button>
      </div>
    </section>
  {/if}
</Sheet>

<style>
  section {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    padding-bottom: var(--s5);
    border-bottom: 1px solid var(--rule);
  }

  section:last-of-type {
    border-bottom: none;
    padding-bottom: 0;
  }

  h3 {
    margin: 0 0 var(--s1);
    color: var(--text-faint);
  }

  .setting {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    min-height: 32px;
  }

  .setting.stack {
    flex-direction: column;
    align-items: stretch;
    gap: var(--s2);
  }

  .control {
    display: flex;
    align-items: center;
    gap: var(--s3);
    flex: 1;
    max-width: 340px;
    justify-content: flex-end;
  }

  /* The three dropdowns share the width the old native selects had. */
  .pick {
    flex: none;
    width: 200px;
  }

  .input {
    cursor: text;
    height: 30px;
    padding: 0 var(--s2);
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius-sm);
    background: var(--surface);
    font-size: var(--t-body);
    min-width: 0;
    flex: 1;
  }

  .narrow {
    flex: none;
    width: 84px;
    text-align: right;
  }

  .value {
    min-width: 44px;
    text-align: right;
    color: var(--text-dim);
    font-size: var(--t-meta);
  }

  .unit {
    flex: none;
  }

  /*
   * The slider. Achromatic, like everything that is not data: an ink fill up to the knob
   * and a hairline track after it.
   */
  input[type="range"] {
    flex: 1;
    max-width: 200px;
    height: 20px;
    appearance: none;
    background: none;
  }

  input[type="range"]::-webkit-slider-runnable-track {
    height: 2px;
    border-radius: 2px;
    background: var(--rule-strong);
  }

  input[type="range"]::-webkit-slider-thumb {
    appearance: none;
    width: 14px;
    height: 14px;
    margin-top: -6px;
    border-radius: 999px;
    background: var(--text);
  }

  .note {
    margin: -4px 0 0;
    max-width: 58ch;
    font-size: var(--t-meta);
    color: var(--text-dim);
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s2);
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .chip {
    display: flex;
    align-items: center;
    gap: var(--s1);
    height: 24px;
    padding: 0 var(--s1) 0 var(--s2);
    border: 1px solid var(--rule);
    border-radius: var(--radius-sm);
    font-size: var(--t-meta);
    color: var(--text-dim);
  }

  .tiny {
    width: 18px;
    height: 18px;
    padding: 0;
    font-size: 14px;
    line-height: 1;
  }

  .about {
    display: grid;
    grid-template-columns: 120px minmax(0, 1fr);
    gap: var(--s1) var(--s3);
    margin: 0;
  }

  .about dt {
    color: var(--text-faint);
  }

  .about dd {
    margin: 0;
  }
</style>
