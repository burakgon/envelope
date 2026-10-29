<script lang="ts">
  import type { Snippet } from 'svelte';
  import { onMount, tick } from 'svelte';
  import { isTyping, restoreFocus, tabbables, trapTab } from '../utils/focus';

  let {
    x,
    y,
    align = 'start',
    width = null,
    over = false,
    role = 'menu',
    label,
    onclose,
    children,
  }: {
    x: number;
    y: number;
    align?: 'start' | 'end' | 'center';
    width?: number | null;
    /** Opened from a dialog: shown above it. */
    over?: boolean;
    /** `dialog` for a popover that tells (a certificate, a birthday) rather than offers items. */
    role?: 'menu' | 'dialog';
    /** Its name for screen readers, as a dialog. */
    label?: string;
    onclose: () => void;
    children: Snippet;
  } = $props();

  let el: HTMLDivElement | undefined = $state();
  let left = $state(-9999);
  let top = $state(-9999);
  let origin = $state('top left');
  /** Below the point or above it; kept while it fits, so that growing content doesn't make it jump. */
  let side: 'below' | 'above' | null = null;
  /** Where the focus was when the menu opened: it goes back there when the menu closes. */
  let before = document.activeElement;
  /** Opened from a field being typed in (a time): the field keeps the keyboard, the popover only offers choices. */
  const fromField = isTyping(before);

  function place() {
    if (!el) return;
    // Its laid-out size: the opening animation scales what getBoundingClientRect() reports.
    const r = { width: el.offsetWidth, height: el.offsetHeight };
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

  /** Moves the focus through the menu's items: the next or previous one, or the first or last. */
  function step(to: 'next' | 'previous' | 'first' | 'last') {
    if (!el) return;
    const items = tabbables(el);
    if (!items.length) return;
    const i = items.indexOf(document.activeElement as HTMLElement);
    const n = items.length;
    const at = to === 'first' ? 0 : to === 'last' ? n - 1 : i < 0 ? (to === 'next' ? 0 : n - 1) : (i + (to === 'next' ? 1 : n - 1)) % n;
    items[at].focus();
  }

  const moves: Record<string, 'next' | 'previous' | 'first' | 'last'> = { ArrowDown: 'next', ArrowUp: 'previous', Home: 'first', End: 'last' };

  /** A field keeps keys it uses: Home and End in any text, the arrows in text areas, lists and dates. */
  function fieldKey(target: Element | null, key: string): boolean {
    if (!isTyping(target)) return false;
    if (key === 'Home' || key === 'End' || !(target instanceof HTMLInputElement)) return true;
    return ['date', 'time', 'datetime-local', 'month', 'week', 'number', 'range'].includes(target.type);
  }

  /** Keys pressed in the menu, after its own fields had them (a filter that moves its highlight with the arrows). */
  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose();
    } else if (e.key === 'Tab') {
      if (el) trapTab(e, el);
    } else if (moves[e.key] && !e.defaultPrevented && !fieldKey(e.target as Element, e.key) && !e.altKey && !e.ctrlKey && !e.metaKey) {
      e.preventDefault();
      step(moves[e.key]);
    }
    // Nothing typed in a menu reaches the shortcuts of the window behind it (j, e, the arrows of the list).
    e.stopPropagation();
  }

  onMount(() => {
    tick().then(() => {
      place();
      // Opened from another menu as it closed: where that one gave the focus back.
      if (!before?.isConnected || before === document.body) before = document.activeElement;
      // A menu takes the focus (on its first item, or a field of its own that already has it).
      if (!fromField && el && !el.contains(document.activeElement)) (tabbables(el)[0] ?? el).focus({ preventScroll: true });
    });
    // Placed again as the content changes size (details that load after it opens) and with the window.
    const resized = new ResizeObserver(() => place());
    if (el) resized.observe(el);
    window.addEventListener('resize', place);
    const down = (e: PointerEvent) => {
      if (el && !el.contains(e.target as Node)) onclose();
    };
    // Keys pressed while the focus is elsewhere (it stayed in a field, or was put back behind the menu).
    const outside = (e: KeyboardEvent) => {
      if (!el || el.contains(e.target as Node)) return;
      if (e.key === 'Escape') {
        e.stopPropagation();
        onclose();
      } else if (moves[e.key] && !fromField) {
        e.preventDefault();
        e.stopPropagation();
        step(e.key === 'ArrowUp' || e.key === 'End' ? 'last' : 'first');
      }
    };
    // Defer so the opening click doesn't immediately close the popover.
    const t = setTimeout(() => window.addEventListener('pointerdown', down, true));
    window.addEventListener('keydown', outside, true);
    window.addEventListener('blur', onclose);
    return () => {
      resized.disconnect();
      window.removeEventListener('resize', place);
      clearTimeout(t);
      window.removeEventListener('pointerdown', down, true);
      window.removeEventListener('keydown', outside, true);
      window.removeEventListener('blur', onclose);
      if (!fromField) restoreFocus(before);
    };
  });
</script>

<div
  bind:this={el}
  class="popover"
  class:over
  {role}
  aria-label={label}
  tabindex="-1"
  style:left="{left}px"
  style:top="{top}px"
  style:width={width ? `${width}px` : null}
  style:--origin={origin}
  onkeydown={key}
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
  /* Above the dialog it was opened from. */
  .popover.over {
    z-index: 1700;
  }
  .popover:focus-visible {
    outline: none;
  }
</style>
