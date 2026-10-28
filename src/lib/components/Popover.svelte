<script lang="ts">
  import type { Snippet } from 'svelte';
  import { onMount, tick } from 'svelte';

  let {
    x,
    y,
    align = 'start',
    width = null,
    onclose,
    children,
  }: {
    x: number;
    y: number;
    align?: 'start' | 'end' | 'center';
    width?: number | null;
    onclose: () => void;
    children: Snippet;
  } = $props();

  let el: HTMLDivElement | undefined = $state();
  let left = $state(-9999);
  let top = $state(-9999);
  let origin = $state('top left');

  async function place() {
    await tick();
    if (!el) return;
    const r = el.getBoundingClientRect();
    const pad = 8;
    let l = align === 'end' ? x - r.width : align === 'center' ? x - r.width / 2 : x;
    let t = y;
    let ox = align === 'end' ? 'right' : 'left';
    let oy = 'top';
    if (l + r.width > innerWidth - pad) l = innerWidth - r.width - pad;
    if (l < pad) l = pad;
    if (t + r.height > innerHeight - pad) {
      t = Math.max(pad, y - r.height - 4);
      oy = 'bottom';
    }
    left = l;
    top = t;
    origin = `${oy} ${ox}`;
  }

  onMount(() => {
    place();
    const down = (e: PointerEvent) => {
      if (el && !el.contains(e.target as Node)) onclose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onclose();
      }
    };
    // Defer so the opening click doesn't immediately close the popover.
    const t = setTimeout(() => window.addEventListener('pointerdown', down, true));
    window.addEventListener('keydown', key, true);
    window.addEventListener('blur', onclose);
    return () => {
      clearTimeout(t);
      window.removeEventListener('pointerdown', down, true);
      window.removeEventListener('keydown', key, true);
      window.removeEventListener('blur', onclose);
    };
  });
</script>

<div
  bind:this={el}
  class="popover"
  role="menu"
  tabindex="-1"
  style:left="{left}px"
  style:top="{top}px"
  style:width={width ? `${width}px` : null}
  style:--origin={origin}
>
  {@render children()}
</div>

<style>
  .popover {
    position: fixed;
    z-index: 1000;
    max-height: calc(100vh - 16px);
    overflow-y: auto;
  }
</style>
