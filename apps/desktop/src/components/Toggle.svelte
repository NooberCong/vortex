<script lang="ts">
  /**
   * A switch.
   *
   * A real `<input type="checkbox">` underneath, so it is focusable, operable by Space,
   * announced correctly and reachable by a label click without a line of script. The
   * styling is a sibling that the input drives; the input itself is invisible rather than
   * absent, which is the difference between a control and a picture of one.
   *
   * On is ink, off is a hairline track. No colour: this is chrome, not data.
   */

  interface Props {
    checked: boolean;
    onchange: (value: boolean) => void;
    label: string;
    disabled?: boolean;
  }

  const { checked, onchange, label, disabled = false }: Props = $props();
</script>

<label class="toggle" class:disabled>
  <input
    type="checkbox"
    {checked}
    {disabled}
    aria-label={label}
    onchange={(e) => onchange(e.currentTarget.checked)}
  />
  <span class="track"><span class="knob"></span></span>
</label>

<style>
  .toggle {
    position: relative;
    display: inline-flex;
    flex: none;
  }

  input {
    position: absolute;
    inset: 0;
    margin: 0;
    opacity: 0;
    cursor: default;
  }

  .track {
    display: block;
    width: 34px;
    height: 20px;
    padding: 2px;
    border: 1px solid var(--rule-strong);
    border-radius: 999px;
    background: var(--bg);
    transition:
      background var(--quick) var(--ease),
      border-color var(--quick) var(--ease);
  }

  .knob {
    display: block;
    width: 14px;
    height: 14px;
    border-radius: 999px;
    background: var(--text-faint);
    transition:
      transform var(--quick) var(--ease),
      background var(--quick) var(--ease);
  }

  input:checked + .track {
    background: var(--text);
    border-color: var(--text);
  }

  input:checked + .track .knob {
    transform: translateX(14px);
    background: var(--surface);
  }

  input:focus-visible + .track {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }

  .disabled {
    opacity: 0.5;
  }
</style>
