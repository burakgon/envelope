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
  /** Below the point or above it; kept while it fits, so that growing content doesn't make it jump. */
  let side: 'below' | 'above' | null = null;

  function place() {
    if (!el) return;
    const r = el.getBoundingClientRect();
    const pad = 8;
    let l = align === 'end' ? x - r.width : align === 'center' ? x - r.width / 2 : x;
    if (l + r.width > innerWidth - pad) l = innerWidth - r.width - pad;
    if (l < pad) l = pad;
    const fits = { below: y + r.height <= innerHeight - pad, above: y - r.height - 4 >= pad };
    if (!side || !fits[side]) side = fits.below || !fits.above ? 'below' : 'above';
    // Taller than the room on either side: as close to the point as the window allows.
    const t = !fits[side] ? Math.max(pad, innerHeight - pad - r.height) : side === 'below' ? y : y - r.height - 4;
    left = l;
    top = t;
    origin = `${side === 'above' ? 'bottom' : 'top'} ${align === 'end' ? 'right' : 'left'}`;
  }

  onMount(() => {
    tick().then(place);
    // Placed again as the content changes size (details that load after it opens) and with the window.
    const resized = new ResizeObserver(() => place());
    if (el) resized.observe(el);
    window.addEventListener('resize', place);
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
      resized.disconnect();
      window.removeEventListener('resize', place);
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
    /* Popovers opened from the composer dock must not inherit its click-through. */
    pointer-events: auto;
    max-height: calc(100vh - 16px);
    overflow-y: auto;
  }
</style>
