<script lang="ts">
  import { cal, addDays, sameDay, startOfWeek } from '../../stores/calendar.svelte';
  import { ChevronLeft, ChevronRight } from '../../icons';
  import { t } from '../../i18n';
  import { dateFormat } from '../../utils/format';

  let {
    value,
    onpick,
    from = null,
    to = null,
    marks = null,
  }: {
    /** The focused day (ms). */
    value: number;
    onpick: (day: number) => void;
    /** Highlighted range [from, to). */
    from?: number | null;
    to?: number | null;
    /** Days (local midnight ms) that get a dot. */
    marks?: Set<number> | null;
  } = $props();

  // The month on display follows `value` unless the user browses away.
  let offset = $state(0);
  let last = NaN;
  $effect(() => {
    if (value !== last) {
      last = value;
      offset = 0;
    }
  });

  const shown = $derived.by(() => {
    const f = new Date(value);
    return new Date(f.getFullYear(), f.getMonth() + offset, 1);
  });
  const days = $derived.by(() => {
    const first = startOfWeek(shown, cal.weekStart);
    return Array.from({ length: 42 }, (_, i) => addDays(first, i));
  });
  const weekdays = $derived(days.slice(0, 7).map((d) => dateFormat({ weekday: 'narrow' }).format(d)));
</script>

<div class="mini">
  <div class="head">
    <span class="label">{dateFormat({ month: 'long', year: 'numeric' }).format(shown)}</span>
    <button class="icon-btn sm" aria-label={t('calendar.miniMonth.previousMonth')} onclick={() => offset--}><ChevronLeft size={15} /></button>
    <button class="icon-btn sm" aria-label={t('calendar.miniMonth.nextMonth')} onclick={() => offset++}><ChevronRight size={15} /></button>
  </div>
  <div class="grid">
    {#each weekdays as w, i (i)}<span class="wd">{w}</span>{/each}
    {#each days as d (d.getTime())}
      {@const ms = d.getTime()}
      <button
        class="d"
        class:other={d.getMonth() !== shown.getMonth()}
        class:today={sameDay(d, Date.now())}
        class:sel={from != null && to != null ? ms >= from && ms < to : sameDay(ms, value)}
        class:single={from == null || to == null}
        class:busy={marks?.has(ms)}
        onclick={() => onpick(ms)}
      >{d.getDate()}</button>
    {/each}
  </div>
</div>

<style>
  .mini {
    padding: 4px 6px 8px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 0 0 6px 6px;
  }
  .label {
    flex: 1;
    font-weight: 650;
    font-size: var(--text-sm);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    row-gap: 1px;
  }
  .wd {
    text-align: center;
    font-size: 10.5px;
    font-weight: 600;
    color: var(--text-3);
    padding-bottom: 3px;
  }
  .d {
    position: relative;
    height: 27px;
    min-width: 27px;
    border-radius: 8px;
    font-size: 12px;
    font-weight: 500;
    font-variant-numeric: tabular-nums;
    color: var(--text-2);
  }
  .d:hover {
    background: var(--bg-hover);
  }
  .d.other {
    color: var(--text-4);
  }
  .d.sel {
    background: var(--bg-selected);
    color: var(--accent-text);
    border-radius: 0;
  }
  .d.sel.single {
    border-radius: 8px;
    background: var(--accent-soft);
  }
  .d.today,
  .d.sel.today {
    background: var(--accent-solid);
    color: var(--accent-fg);
    font-weight: 700;
    border-radius: 8px;
  }
  .d.busy::after {
    content: '';
    position: absolute;
    left: 50%;
    bottom: 2px;
    width: 3px;
    height: 3px;
    margin-left: -1.5px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.55;
  }
</style>
