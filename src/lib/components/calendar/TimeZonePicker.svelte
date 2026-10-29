<script lang="ts">
  import { tick } from 'svelte';
  import { t } from '../../i18n';
  import { localZone, sameZone, zoneChoices, zoneLabel, type ZoneChoice } from '../../utils/timezone';
  import Popover from '../Popover.svelte';
  import { Check, Globe, Search } from '../../icons';

  let {
    value = $bindable(),
    onchange,
    variant = 'field',
    label = null,
  }: {
    /** An IANA zone. */
    value: string;
    onchange?: (tz: string) => void;
    /** `link`: quiet text (the event editor); `field`: like a select (the settings). */
    variant?: 'link' | 'field';
    /** What the button says instead of the zone. */
    label?: string | null;
  } = $props();

  let open = $state<{ x: number; y: number } | null>(null);
  let query = $state('');
  let active = $state(0);
  let input: HTMLInputElement | undefined = $state();
  let list: HTMLDivElement | undefined = $state();

  const here = localZone();
  const text = $derived(label ?? zoneLabel(value));

  // This computer's zone first, then every zone west to east.
  const shown = $derived.by((): ZoneChoice[] => {
    if (!open) return [];
    const all = zoneChoices();
    const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const matching = all.filter((z) => words.every((w) => z.search.includes(w)));
    const own = matching.find((z) => z.id === here);
    return own ? [own, ...matching.filter((z) => z !== own)] : matching;
  });

  async function show(e: MouseEvent) {
    if (open) {
      open = null;
      return;
    }
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    query = '';
    open = { x: r.left, y: r.bottom + 4 };
    await tick();
    active = Math.max(0, shown.findIndex((z) => sameZone(z.id, value)));
    input?.focus();
    reveal('center');
  }

  function reveal(block: ScrollLogicalPosition = 'nearest') {
    list?.querySelector<HTMLElement>(`[data-i="${active}"]`)?.scrollIntoView({ block });
  }

  function choose(z: ZoneChoice) {
    value = z.id;
    open = null;
    onchange?.(z.id);
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      active = Math.max(0, Math.min(shown.length - 1, active + (e.key === 'ArrowDown' ? 1 : -1)));
      reveal();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      e.stopPropagation();
      if (shown[active]) choose(shown[active]);
    }
  }
</script>

<button type="button" class="tz {variant}" class:open={!!open} aria-haspopup="listbox" aria-expanded={!!open} onclick={show}>
  {#if variant === 'link'}<Globe size={14} />{/if}<span class="truncate">{text}</span>
</button>

{#if open}
  <Popover x={open.x} y={open.y} onclose={() => (open = null)} width={340}>
    <div class="picker">
      <label class="search">
        <Search size={14} />
        <input
          bind:this={input}
          bind:value={query}
          placeholder={t('calendar.timeZone.search')}
          aria-label={t('calendar.timeZone.search')}
          spellcheck="false"
          oninput={() => (active = 0)}
          onkeydown={key}
        />
      </label>
      <div class="list" role="listbox" aria-label={t('calendar.editor.timeZone')} bind:this={list}>
        {#each shown as z, i (z.id)}
          {@const chosen = sameZone(z.id, value)}
          <button
            type="button"
            class="zone"
            class:active={i === active}
            role="option"
            aria-selected={chosen}
            data-i={i}
            title={z.id}
            onpointermove={() => (active = i)}
            onclick={() => choose(z)}
          >
            <span class="check">{#if chosen}<Check size={14} />{/if}</span>
            <span class="names">
              <span class="city truncate">{z.city}{#if z.id === here}<span class="here">{t('calendar.timeZone.thisComputer')}</span>{/if}</span>
              <span class="name truncate">{z.name}</span>
            </span>
            <span class="off">{z.offset}</span>
          </button>
        {:else}
          <div class="none">{t('calendar.timeZone.noMatch', { query: query.trim() })}</div>
        {/each}
      </div>
    </div>
  </Popover>
{/if}

<style>
  .tz {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    max-width: 100%;
  }
  .tz.link {
    height: 28px;
    padding: 0 8px;
    margin-left: -8px;
    border-radius: 8px;
    color: var(--text-2);
    font-weight: 500;
  }
  .tz.link:hover,
  .tz.link.open {
    color: var(--text);
    background: var(--bg-hover);
  }
  .tz.field {
    height: 34px;
    padding: 0 12px;
    border-radius: var(--r);
    font-weight: 500;
    background: var(--bg-input);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .tz.field:hover {
    box-shadow: inset 0 0 0 1px var(--text-4);
  }
  .tz.field.open {
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .picker {
    display: flex;
    flex-direction: column;
    max-height: min(420px, 70vh);
    overflow: hidden;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 10px;
    height: 38px;
    flex-shrink: 0;
    color: var(--text-3);
    border-bottom: 1px solid var(--border);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: none;
    background: none;
    color: var(--text);
  }
  .list {
    overflow-y: auto;
    padding: 4px;
  }
  .zone {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 6px 8px;
    border-radius: 8px;
    text-align: left;
  }
  .zone.active {
    background: var(--bg-hover);
  }
  .check {
    display: grid;
    width: 14px;
    flex-shrink: 0;
    color: var(--accent-text);
  }
  .names {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .city {
    font-weight: 600;
    color: var(--text);
  }
  .here {
    margin-left: 8px;
    font-size: 11px;
    font-weight: 600;
    color: var(--accent-text);
  }
  .name {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .off {
    flex-shrink: 0;
    font-size: var(--text-sm);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .none {
    padding: 14px 10px;
    color: var(--text-3);
    font-size: var(--text-sm);
  }
</style>
