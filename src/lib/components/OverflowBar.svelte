<script lang="ts" module>
  import type { Archive } from '../icons';

  /** A toolbar button; what doesn't fit goes into the toolbar's More menu, from the end. */
  export interface BarAction {
    id: string;
    /** The tooltip (with the shortcut) and the name for screen readers. */
    title: string;
    /** The text in the More menu (else the title). */
    label?: string;
    /** The shortcut, beside the text in the More menu. */
    key?: string;
    icon: typeof Archive;
    /** Runs it; `at` is where a menu it opens goes (below its button, or the More button). */
    run: (at: { x: number; y: number }) => void;
    active?: boolean;
    /** Begins a group: a separator comes before it. */
    group?: boolean;
  }
</script>

<script lang="ts">
  import Popover from './Popover.svelte';
  import { More } from '../icons';
  import { t } from '../i18n';

  let { actions }: { actions: BarAction[] } = $props();

  let width = $state(0);
  let menu = $state<{ x: number; y: number } | null>(null);

  // An icon button is 30px wide with 2px beside it; a separator takes 15px.
  const BUTTON = 32;
  const SEPARATOR = 15;
  const cost = (i: number) => BUTTON + (i > 0 && actions[i].group ? SEPARATOR : 0);
  /** How many buttons fit; the rest go into More (which takes a button's room). */
  const fits = $derived.by(() => {
    if (actions.reduce((n, _, i) => n + cost(i), 0) <= width) return actions.length;
    let room = width - BUTTON;
    let n = 0;
    while (n < actions.length && room >= cost(n)) room -= cost(n++);
    return n;
  });

  function below(el: HTMLElement) {
    const r = el.getBoundingClientRect();
    return { x: r.left, y: r.bottom + 4 };
  }
</script>

<div class="bar" bind:clientWidth={width} data-tauri-drag-region>
  {#each actions.slice(0, fits) as a, i (a.id)}
    {#if i > 0 && a.group}<span class="sep"></span>{/if}
    <button class="icon-btn" class:active={a.active} title={a.title} aria-label={a.title} onclick={(e) => a.run(below(e.currentTarget))}><a.icon size={17} /></button>
  {/each}
  {#if fits < actions.length}
    <button
      class="icon-btn more-btn"
      title={t('common.more')}
      aria-label={t('common.more')}
      aria-haspopup="menu"
      aria-expanded={!!menu}
      onclick={(e) => (menu = menu ? null : below(e.currentTarget))}><More size={17} /></button>
  {/if}
</div>

{#if menu}
  <Popover x={menu.x} y={menu.y} onclose={() => (menu = null)} width={250}>
    {#each actions.slice(fits) as a (a.id)}
      <button
        class="menu-item"
        class:on={a.active}
        onclick={() => {
          const at = menu!;
          menu = null;
          a.run(at);
        }}><a.icon size={15} /><span class="truncate">{a.label ?? a.title}</span>{#if a.key}<span class="hint">{a.key}</span>{/if}</button>
    {/each}
  </Popover>
{/if}

<style>
  .bar {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 2px;
    overflow: hidden;
  }
  .sep {
    flex-shrink: 0;
    width: 1px;
    height: 18px;
    margin: 0 6px;
    background: var(--border-strong);
  }
  .menu-item.on {
    color: var(--accent-text);
  }
</style>
