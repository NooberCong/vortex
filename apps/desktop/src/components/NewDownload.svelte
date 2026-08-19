<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { bytes } from "@vortex/proto";
  import type { Category } from "@vortex/proto";

  import * as act from "$lib/actions";
  import { native } from "$lib/ipc";
  import { queue } from "$lib/store.svelte";
  import Select from "./Select.svelte";
  import Sheet from "./Sheet.svelte";

  /**
   * New Download (05 §Screens).
   *
   * Two steps, because the first one is a question the app can actually answer. A URL goes
   * to the daemon, the daemon runs the real probe from 02 §2, and what comes back is the
   * truth about the file — its size, its name after redirects, and whether the server will
   * honour a range request. So the sheet can say `4.9 GB · releases.ubuntu.com · resumable`
   * *before* the user commits, and say `single stream` when it will not be, because that
   * changes what they should expect.
   *
   * Guessing any of that from the URL would be faster and would occasionally be wrong,
   * which is worse than waiting a moment.
   */

  interface Props {
    onClose: () => void;
  }

  const { onClose }: Props = $props();

  const CATEGORIES = (
    ["Video", "Audio", "Archives", "Documents", "Images", "Programs", "Other"] as Category[]
  ).map((name) => ({ value: name, label: name }));

  let url = $state("");
  let probing = $state(false);
  let destDir = $state("");
  let category = $state<Category | null>(null);

  const result = $derived(queue.probed);

  // The daemon answered. Adopt its suggestions as the starting point — they are the
  // settings the user already chose, resolved for this file's category.
  $effect(() => {
    if (!result) return;
    probing = false;
    destDir ||= result.suggestedDir ?? queue.settings?.downloadDir ?? "";
    category ??= result.category;
  });

  async function look(): Promise<void> {
    const trimmed = url.trim();
    if (!trimmed) return;
    probing = true;
    queue.probed = null;
    await act.probe(trimmed);
  }

  async function pick(): Promise<void> {
    if (!native) return;
    const chosen = await open({ directory: true, defaultPath: destDir || undefined });
    if (typeof chosen === "string") destDir = chosen;
  }

  async function start(startPaused: boolean): Promise<void> {
    if (!result) return;
    await act.submit({
      envelope: {
        url: result.url,
        finalUrl: result.finalUrl,
        method: "GET",
        headers: [],
        cookies: null,
        bodyBase64: null,
        mimeType: result.mimeType,
        contentLength: result.size,
        filenameHint: result.filename,
        tabId: null,
        pageUrl: null,
        pageTitle: null,
        capturedAt: Math.floor(Date.now() / 1000),
      },
      destDir: destDir || null,
      filename: null,
      category,
      priority: "Normal",
      startPaused,
      media: null,
    });
    queue.probed = null;
    onClose();
  }
</script>

<Sheet title="New download" {onClose}>
  {#if !result}
    <label class="field">
      <span class="micro">Link</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="input num"
        type="url"
        autofocus
        placeholder="https://"
        bind:value={url}
        onkeydown={(e) => e.key === "Enter" && look()}
      />
    </label>
    <p class="hint meta">
      Paste a link, or drop one anywhere on the window. Vortex catches downloads from your
      browser without being asked.
    </p>
    <div class="actions">
      <button class="button primary" disabled={!url.trim() || probing} onclick={look}>
        {probing ? "Checking…" : "Continue"}
      </button>
    </div>
  {:else}
    <div class="file">
      <p class="filename">{result.filename}</p>
      <p class="facts meta num">
        {result.size ? bytes(result.size) : "size unknown"}
        <span class="dot">·</span>{result.host}
        <!-- Straight from the probe. `single stream` is not a warning, it is what to
             expect: one connection, and a resume that has to start over. -->
        <span class="dot">·</span>{result.resumable ? "resumable" : "single stream"}
      </p>
    </div>

    <label class="field row">
      <span class="micro">Save to</span>
      <input class="input num" bind:value={destDir} spellcheck="false" />
      <button class="button" onclick={pick} disabled={!native}>Browse</button>
    </label>

    <!-- A div, not a label: a label wrapping a control that opens a menu forwards the
         click twice, and the menu opens and shuts. -->
    <div class="field row">
      <span class="micro">Category</span>
      <Select
        label="Category"
        options={CATEGORIES}
        value={category ?? "Other"}
        onchange={(chosen) => (category = chosen)}
      />
    </div>

    <div class="actions">
      <button class="button" onclick={() => start(true)}>Add to queue</button>
      <button class="button primary" onclick={() => start(false)}>Download</button>
    </div>
  {/if}
</Sheet>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }

  .field.row {
    display: grid;
    grid-template-columns: 88px minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--s3);
  }

  .field .micro {
    color: var(--text-faint);
  }

  .input {
    cursor: text;
    height: 32px;
    padding: 0 var(--s3);
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius-sm);
    background: var(--surface);
    font-size: var(--t-body);
    min-width: 0;
  }

  .hint {
    margin: 0;
    max-width: 52ch;
  }

  .file {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
  }

  .filename {
    margin: 0;
    font-size: var(--t-name);
    font-weight: 500;
    word-break: break-all;
  }

  .facts {
    margin: 0;
  }

  .dot {
    padding: 0 var(--s2);
    color: var(--text-faint);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--s2);
    padding-top: var(--s2);
    border-top: 1px solid var(--rule);
  }
</style>
