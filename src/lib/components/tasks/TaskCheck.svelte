<script lang="ts">
  import { untrack } from 'svelte';

  let {
    checked,
    color,
    label,
    size = 18,
    onclick,
  }: {
    checked: boolean;
    /** The list's color. */
    color: string;
    label: string;
    size?: number;
    onclick: () => void;
  } = $props();

  // Pops when checked here, not when shown already checked.
  let was = untrack(() => checked);
  let pop = $state(false);
  $effect.pre(() => {
    pop = checked && (pop || !was);
    was = checked;
  });
</script>

<!-- Not a tab stop: in lists the keyboard completes with X; elsewhere it sits next to fields that are. -->
<button
  class="check"
  class:checked
  class:pop
  role="checkbox"
  aria-checked={checked}
  aria-label={label}
  title={label}
  tabindex="-1"
  style:--c={color}
  style:--s="{size}px"
  onclick={(e) => {
    e.stopPropagation();
    onclick();
  }}
>
  <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4.2 8.4 6.9 11 11.8 5.6" /></svg>
</button>

<style>
  .check {
    position: relative;
    display: grid;
    place-items: center;
    width: var(--s);
    height: var(--s);
    margin-top: 1px;
    border-radius: 50%;
    flex-shrink: 0;
    color: #fff;
    box-shadow: inset 0 0 0 1.5px color-mix(in oklab, var(--c) 75%, var(--text-3));
    transition: background var(--dur) var(--ease), box-shadow var(--dur) var(--ease), transform var(--dur-slow) var(--spring);
  }
  /* A wider target than the circle. */
  .check::before {
    content: '';
    position: absolute;
    inset: -6px;
    border-radius: 50%;
  }
  .check:hover {
    background: color-mix(in oklab, var(--c) 14%, transparent);
  }
  .check.checked {
    background: var(--c);
    box-shadow: inset 0 0 0 1.5px var(--c);
  }
  .check.pop {
    animation: pop var(--dur-slow) var(--spring);
  }
  svg {
    width: 72%;
    height: 72%;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.2;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-dasharray: 12;
    stroke-dashoffset: 12;
    transition: stroke-dashoffset 220ms var(--ease-out) 60ms;
  }
  .check:not(.checked):hover svg {
    stroke: color-mix(in oklab, var(--c) 70%, var(--text-3));
    stroke-dashoffset: 0;
    opacity: 0.55;
    transition: none;
  }
  .checked svg {
    stroke-dashoffset: 0;
  }
  @keyframes pop {
    40% {
      transform: scale(1.18);
    }
  }
</style>
