<script lang="ts">
  import { tick } from 'svelte';
  import { t } from '../../i18n';
  import { timeOf } from '../../utils/format';
  import Popover from '../Popover.svelte';

  let {
    value = $bindable(),
    onchange,
    after = null,
  }: {
    /** "HH:mm", 24-hour. */
    value: string;
    onchange?: () => void;
    /** For an end time on the same day: list starts after this "HH:mm" and shows durations. */
    after?: string | null;
  } = $props();

  const pad = (n: number) => String(n).padStart(2, '0');
  const toMin = (v: string) => {
    const [h, m] = v.split(':').map(Number);
    return h * 60 + m;
  };
  const fromMin = (m: number) => `${pad(Math.floor(m / 60) % 24)}:${pad(m % 60)}`;
  const fmt = (m: number) => (Number.isFinite(m) ? timeOf(new Date(2000, 0, 1, Math.floor(m / 60), m % 60)) : '');

  function duration(mins: number): string {
    if (mins < 60) return t('calendar.picker.durationMinutes', { count: mins });
    return t('calendar.picker.durationHours', { count: mins / 60 });
  }

  /** Accepts "9", "930", "9:30", "9.30pm", "21:30", "9 am", "9 a.m.", ... */
  function parse(text: string): number | null {
    const s = text
      .trim()
      .toLowerCase()
      .replace(/\s+/g, '')
      .replace(/([ap])\.m\.?$/, '$1m');
    const m = s.match(/^(\d{1,2})(?:[:.h]?(\d{2}))?(a|am|p|pm)?$/);
    if (!m) return null;
    let h = Number(m[1]);
    const min = Number(m[2] ?? 0);
    const ap = m[3];
    if (min > 59) return null;
    if (ap) {
      if (h < 1 || h > 12) return null;
      if (ap.startsWith('p') && h !== 12) h += 12;
      if (ap.startsWith('a') && h === 12) h = 0;
    } else if (h > 23) return null;
    return h * 60 + min;
  }

  let text = $state('');
  let open = $state<{ x: number; y: number; w: number } | null>(null);
  let input: HTMLInputElement | undefined = $state();
  let list: HTMLDivElement | undefined = $state();
  let active = $state(-1);

  $effect(() => {
    text = fmt(toMin(value));
  });

  const options = $derived.by(() => {
    const base = after != null ? toMin(after) : null;
    const out: { m: number; label: string; hint: string | null }[] = [];
    if (base != null) {
      for (let d = 15; base + d < 24 * 60; d += d < 60 ? 15 : 30) out.push({ m: base + d, label: fmt(base + d), hint: duration(d) });
    } else {
      for (let m = 0; m < 24 * 60; m += 15) out.push({ m, label: fmt(m), hint: null });
    }
    return out;
  });

  async function show() {
    if (open || !input) return;
    const r = input.getBoundingClientRect();
    open = { x: r.left, y: r.bottom + 4, w: Math.max(r.width, after != null ? 170 : 120) };
    const cur = toMin(value);
    active = options.findIndex((o) => o.m >= cur);
    await tick();
    list?.querySelector<HTMLElement>(`[data-i="${Math.max(0, active)}"]`)?.scrollIntoView({ block: 'center' });
  }

  function choose(m: number) {
    value = fromMin(m);
    text = fmt(m);
    open = null;
    onchange?.();
  }

  function commit() {
    const m = parse(text);
    if (m == null) text = fmt(toMin(value));
    else if (fromMin(m) !== value) choose(m);
    else text = fmt(m);
  }

  async function key(e: KeyboardEvent) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (!open) await show();
      active = Math.max(0, Math.min(options.length - 1, active + (e.key === 'ArrowDown' ? 1 : -1)));
      list?.querySelector<HTMLElement>(`[data-i="${active}"]`)?.scrollIntoView({ block: 'nearest' });
    } else if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      // Let the editor save, with whatever was typed.
      commit();
      open = null;
    } else if (e.key === 'Enter') {
      e.preventDefault();
      e.stopPropagation();
      // Untouched text (in whatever format the language uses) picks the highlighted option.
      if (open && active >= 0 && (text === fmt(toMin(value)) || parse(text) === toMin(value))) choose(options[active].m);
      else {
        commit();
        open = null;
      }
    } else if (e.key === 'Escape' && open) {
      e.stopPropagation();
      open = null;
      text = fmt(toMin(value));
    } else if (e.key === 'Tab') {
      open = null;
    }
  }
</script>

<input
  bind:this={input}
  class="field"
  class:open={!!open}
  bind:value={text}
  onfocus={show}
  onclick={show}
  onblur={commit}
  onkeydown={key}
  spellcheck="false"
  aria-label={t('calendar.picker.time')}
/>

{#if open}
  <Popover x={open.x} y={open.y} width={open.w} onclose={() => (open = null)}>
    <div class="list" bind:this={list}>
      {#each options as o, i (o.m)}
        <button
          data-i={i}
          class="opt"
          class:active={i === active}
          class:cur={fromMin(o.m) === value}
          onpointerdown={(e) => e.preventDefault()}
          onclick={() => choose(o.m)}
        >
          <span>{o.label}</span>{#if o.hint}<span class="hint">{o.hint}</span>{/if}
        </button>
      {/each}
    </div>
  </Popover>
{/if}

<style>
  .field {
    width: 104px;
    height: 34px;
    padding: 0 12px;
    border: none;
    border-radius: var(--r);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
    background: var(--bg-input);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition: box-shadow var(--dur-fast);
  }
  .field:hover {
    box-shadow: inset 0 0 0 1px var(--text-4);
  }
  .field:focus,
  .field.open {
    outline: none;
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .list {
    max-height: 260px;
    overflow-y: auto;
    padding: 2px;
  }
  .opt {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    width: 100%;
    height: 30px;
    padding: 0 10px;
    border-radius: 7px;
    font-variant-numeric: tabular-nums;
    text-align: left;
  }
  .opt:hover,
  .opt.active {
    background: var(--bg-hover);
  }
  .opt.cur {
    color: var(--accent-text);
    font-weight: 600;
  }
  .hint {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
</style>
