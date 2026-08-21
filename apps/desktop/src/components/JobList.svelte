<script lang="ts">
  import { CALM, exit, fade } from "$lib/motion";
  import { queue } from "$lib/store.svelte";
  import EmptyState from "./EmptyState.svelte";
  import JobRow from "./JobRow.svelte";

  /**
   * The list.
   *
   * One list, in three headed sections — active, then queued, then done (05 §Layout). The
   * order is the store's (`Band`); this draws it.
   *
   * Not virtualised, deliberately. The quality floor is 60 fps with forty jobs listed
   * (05 §Quality floor), and forty rows of three lines each is nothing for a DOM — the
   * frame budget goes on the canvases, which are gated by `IntersectionObserver` and
   * already only draw when they are on screen. Virtualising would add a scroll-position
   * bug surface to save layout work that is not the bottleneck.
   *
   * A band appears the first time something is in it and goes when the last thing leaves,
   * and it fades either way. Only the heading, not the rows under it: the rows are folding
   * on their own clock, and a band that slid away as a block would take rows with it that
   * are still there — the last active download becoming the first finished one is one row
   * moving between two headings, not a section closing.
   *
   * **Forty is now a cap rather than an observation**, which is what the sentinel below is
   * for. While Done was its own filter nobody ever had a long list open: you do
   * not have two hundred active downloads. One list ends with every download ever
   * finished, so it renders a page at a time and asks for the next one when the bottom
   * comes into view. There is nothing to fetch — every job is already in this process —
   * so there is no spinner and no loading state, only more rows.
   */

  const sections = $derived(queue.sections);
  const filtered = $derived(queue.jobs.length > 0 || queue.search.length > 0);

  const HEADINGS = { active: "Active", queued: "Queued", done: "Done" } as const;

  let list: HTMLElement | null = $state(null);
  let sentinel: HTMLElement | null = $state(null);

  // Keyboard navigation moves `selected`; this is what makes the row it moved to visible.
  // `nearest` rather than `center` so a row already on screen does not make the list jump.
  $effect(() => {
    const id = queue.selected;
    if (id === null || !list) return;
    list.querySelector<HTMLElement>(`[data-job="${id}"]`)?.scrollIntoView({ block: "nearest" });
  });

  /**
   * The next page, when the end of this one is reached.
   *
   * `rootMargin` runs it a screen early, so the rows are there before the scroll arrives at
   * where they go — the alternative is a list that stops at the bottom and then jerks.
   *
   * The observer is re-created whenever the sentinel is replaced, which is what happens
   * when the last page is rendered and it stops existing. `queue.more` is idempotent once
   * there is no rest, so a callback that fires twice on the way past costs nothing.
   */
  $effect(() => {
    const target = sentinel;
    if (!target || !list) return;
    const watch = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) queue.more();
      },
      { root: list, rootMargin: "50% 0px" },
    );
    watch.observe(target);
    return () => watch.disconnect();
  });
</script>

<div class="list scroll" bind:this={list}>
  {#if sections.length === 0}
    <div class="nothing" in:fade={{ duration: CALM }}>
      <EmptyState {filtered} />
    </div>
  {:else}
    {#each sections as section (section.band)}
      <section
        in:fade={{ duration: CALM }}
        out:fade={{ duration: CALM, easing: exit }}
      >
        <!--
          Sticky, because the whole point of one list is that you scroll through it, and a
          heading that scrolled away would leave a screen of rows with nothing saying which
          band they are. It is a hairline and an eyebrow, not a bar: this is a label for
          what is under it, not a control.
        -->
        <h2 class="heading">
          <span class="micro">{HEADINGS[section.band]}</span>
          <span class="count num">{section.total}</span>
        </h2>
        <ul>
          {#each section.jobs as job (job.id)}
            <JobRow {job} />
          {/each}
        </ul>
      </section>
    {/each}

    {#if queue.rest > 0}
      <!--
        Mostly this is never seen, and that is the intent: the observer above runs a screen
        early, so the next page is in place before the scroll arrives here. It is a button
        rather than a bare sentinel for the case where that does not happen at all — no
        `IntersectionObserver`, or a callback that never fires — because a list whose end
        can only be reached by a working observer is a list that can silently become
        unfinishable. When it *is* seen it says what is left rather than asking for a
        decision, since scrolling one more line will do the same thing.
      -->
      <button
        class="more"
        bind:this={sentinel}
        onclick={() => queue.more()}
        in:fade={{ duration: CALM }}
      >
        {queue.rest} more
      </button>
    {/if}
  {/if}
</div>

<style>
  .list {
    flex: 1;
    min-width: 0;
    background: var(--surface);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  /*
   * A wrapper only so the sentence can fade in. It arrives after the last row folded away,
   * or after a search stopped matching, and in both cases it is answering something the
   * user just did — appearing between two frames reads as the list breaking.
   */
  .nothing {
    height: 100%;
  }

  .heading {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s3);
    margin: 0;
    /* `--s5` across, so the eyebrow starts on the same column as the filenames. */
    padding: var(--s3) var(--s5) var(--s2);
    /* Opaque, not translucent: rows pass underneath it and a blur here would be a second
       compositing layer over the one thing in the app that is already animating. */
    background: var(--surface);
    border-bottom: 1px solid var(--rule);
    color: var(--text-dim);
  }

  .count {
    font-size: var(--t-meta);
    color: var(--text-faint);
  }

  .more {
    display: block;
    width: 100%;
    padding: var(--s4);
    color: var(--text-faint);
    font-size: var(--t-meta);
    transition: color var(--quick) var(--ease);
  }

  .more:hover {
    color: var(--text-dim);
  }
</style>
